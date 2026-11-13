//! `ChangeMark`: what someone else changed that the person has not seen yet (task-legibility §3), as
//! one pill in the changer's colour: their face, and the task run it was part of.

use crate::component::{self, Component, Event, Person, ResourceId};
use crate::tokens::Rhythm;
use crate::tokens::colors::PresenceHue;
use crate::widget::{Button, Chip, ChipStyle, Container, Element, Tone};

/// What someone else changed of a resource that the person looking has not seen yet: who, in which task
/// run, and whether they made, changed or removed it.
#[derive(Component, Default)]
#[component(name = "ChangeMark", role = "Button", label = "run")]
pub struct ChangeMarkProps {
    /// The resource whose changes it marks.
    #[component(positional)]
    pub of: Option<ResourceId>,
    /// Who made the newest change the person has not seen.
    pub by: Option<Person>,
    /// Everyone who made one of the changes the person has not seen, in the order they changed it (its
    /// newest change last), each once.
    pub by_all: Vec<Person>,
    /// What the newest change did, in the words history says it with ("moved this to").
    pub said: Option<String>,
    /// What one item of it is called, plural ("tasks"): the noun a window's count names.
    pub noun: Option<String>,
    /// What one item of it is called, singular ("task"): one change names one, so the count and the noun
    /// agree in number.
    pub noun_one: Option<String>,
    /// The task run it was part of, as its task is titled.
    pub run: Option<String>,
    /// What they did: `created`, `changed` or `removed`.
    #[component(data)]
    pub kind: Option<String>,
    /// How many changes of it the person has not seen.
    pub count: i64,
    /// It marks a window, in its header: it says how many changes and by whom.
    pub window: bool,
    /// Every change it counts only put items in (`$noun`): what it says is new rather than changed.
    pub added: bool,
    /// The window's document holds more resources than the mark counted: what it says is a lower bound, and
    /// says so.
    pub more: bool,
    /// Activated: show what they did, at the run that did it.
    #[component(event)]
    pub activate: Event,
    /// What a window's mark opens: the views of its catch-up (*Since you looked*), placed by whoever placed
    /// the mark.
    #[component(slot(default))]
    pub content: Vec<Element>,
}

pub struct RowMark {
    pub identity: String,
    pub name: String,
    pub spoken: String,
    pub said: String,
    pub show: Option<Box<dyn FnMut()>>,
}

impl ChangeMarkProps {
    /// The same mark again, its activation shared: a row is built anew each time it scrolls into view.
    #[must_use]
    pub fn again(&self) -> Self {
        Self {
            of: self.of.clone(),
            by: self.by.clone(),
            by_all: self.by_all.clone(),
            said: self.said.clone(),
            noun: self.noun.clone(),
            noun_one: self.noun_one.clone(),
            run: self.run.clone(),
            kind: self.kind.clone(),
            count: self.count,
            window: self.window,
            added: self.added,
            more: self.more,
            activate: self.activate.clone(),
            content: Vec::new(),
        }
    }

    #[must_use]
    pub fn row(self) -> Option<RowMark> {
        let by = self.by?;
        let did = match self.kind.as_deref() {
            Some("created") => "change-mark-created",
            Some("removed") => "change-mark-removed",
            _ => "change-mark-changed",
        };
        let what = crate::i18n::widget_format(did, &[("who", by.name.clone().into())]);
        let spoken = match self.run.filter(|run| !run.is_empty()) {
            Some(run) => crate::i18n::widget_format(
                "change-mark-in-run",
                &[("what", what.into()), ("run", run.into())],
            ),
            None => what,
        };
        let said = self
            .said
            .filter(|said| !said.is_empty())
            .map_or_else(String::new, |said| {
                let line = crate::i18n::widget_format(
                    "change-mark-said",
                    &[("who", by.name.clone().into()), ("said", said.into())],
                );
                let also = self.count.saturating_sub(1);
                if also > 0 {
                    crate::i18n::widget_format(
                        "change-mark-said-more",
                        &[("said", line.into()), ("count", also.into())],
                    )
                } else {
                    line
                }
            });
        Some(RowMark {
            identity: by.identity,
            name: by.name,
            spoken,
            said,
            show: self.activate.handler(),
        })
    }

    #[must_use]
    pub fn face(self, key: &str) -> Option<Chip> {
        let mark = self.row()?;
        let hue = PresenceHue::of_identity(&mark.identity);
        let chip = Chip::new(String::new())
            .with_id(key.to_owned())
            .with_face(mark.name.clone(), face_colour(hue))
            .style(ChipStyle::Tinted(Tone::Presence(hue)))
            .tooltip(mark.spoken.clone())
            .with_accessible_name(mark.spoken);
        Some(match mark.show {
            Some(mut show) => chip.on_click(move || show()),
            None => chip,
        })
    }

    #[must_use]
    pub fn chip(&self, key: &str) -> Option<Element> {
        Some(Box::new(self.pill(key)?))
    }

    #[must_use]
    pub fn catch_up(&mut self, key: &str) -> Option<Element> {
        let views = std::mem::take(&mut self.content);
        if !self.window || views.is_empty() {
            return None;
        }
        let button = self.pill(key)?;
        let mut column = crate::widget::arrange::Stack::stack()
            .fill_width()
            .gap(Rhythm::Related);
        for view in views {
            column = column.push(view);
        }
        let content = Container::new()
            .with_no_padding()
            .with_child(Box::new(column));
        let mut pair = button.with_popover(content);
        pair.popover_mut().set_confined_to_viewport(true);
        pair.popover_mut()
            .set_max_width(f64::from(crate::tokens::Metrics::BASE.panel_width_list));
        pair.popover_mut().set_sheet_when_narrow(true);
        Some(Box::new(pair))
    }

    fn pill(&self, key: &str) -> Option<Button> {
        let by = self.by.clone()?;
        let by_all = self.by_all.clone();
        let noun = self.noun.clone();
        let noun_one = self.noun_one.clone();
        let count = self.count;
        let window = self.window;
        let added = self.added;
        let more = self.more;
        let activate = self.activate.clone();
        let mut who: Vec<Person> = Vec::new();
        for person in by_all.into_iter().chain(std::iter::once(by)) {
            if !who.iter().any(|seen| seen.identity == person.identity) {
                who.push(person);
            }
        }
        let faces: Vec<(String, String)> = if window {
            who.iter()
                .map(|person| (person.name.clone(), person.identity.clone()))
                .collect()
        } else {
            who.iter()
                .take(1)
                .map(|person| (person.name.clone(), person.identity.clone()))
                .collect()
        };
        let count = count.max(1);
        let label = if window {
            let names = names(&who);
            match noun.filter(|noun| !noun.is_empty()) {
                Some(noun) => crate::i18n::widget_format(
                    match (added, more) {
                        (true, false) => "change-window-new",
                        (true, true) => "change-window-new-more",
                        (false, true) => "change-window-changed-more",
                        (false, false) => "change-window-changed",
                    },
                    &[
                        ("who", names.into()),
                        ("count", count.into()),
                        ("noun", noun.clone().into()),
                        // One change names one item, so the sentence agrees in number: the
                        // singular noun the type's own catalogue gives, the plural otherwise.
                        ("one", noun_one.unwrap_or(noun).into()),
                    ],
                ),
                None => crate::i18n::widget_format(
                    if more {
                        "change-window-more"
                    } else {
                        "change-window"
                    },
                    &[("count", count.into()), ("who", names.into())],
                ),
            }
        } else {
            let name = who
                .first()
                .map_or_else(String::new, |person| person.name.clone());
            crate::i18n::widget_format("change-mark-who", &[("who", name.into())])
        };
        let mut button = Button::new(key, &label)
            .role(crate::widget::ButtonRole::Plain)
            .with_faces_of(faces)
            .with_accessible_name(&label)
            .tooltip(label.clone());
        button = match activate {
            Event::None => button,
            Event::Action(binding) => button.on_action(binding),
            Event::View(intent) => button.on_view(intent.sender()),
        };
        Some(button)
    }
}

/// What `people` are called, as a sentence names them: "Bo", "Bo and Scout", "Bo, Scout and Kit",
/// and past three "Bo, Scout and 3 others" — the faces count the rest.
fn names(people: &[Person]) -> String {
    let name = |index: usize| -> String { people[index].name.clone() };
    match people.len() {
        0 => String::new(),
        1 => name(0),
        2 => crate::i18n::widget_format(
            "change-mark-names-two",
            &[("first", name(0).into()), ("second", name(1).into())],
        ),
        3 => crate::i18n::widget_format(
            "change-mark-names-three",
            &[
                ("first", name(0).into()),
                ("second", name(1).into()),
                ("third", name(2).into()),
            ],
        ),
        more => crate::i18n::widget_format(
            "change-mark-names-more",
            &[
                ("first", name(0).into()),
                ("second", name(1).into()),
                ("count", i64::try_from(more - 2).unwrap_or(i64::MAX).into()),
            ],
        ),
    }
}

/// The colour a face takes for `hue`: each theme paints it at its own presence lightness.
fn face_colour(hue: PresenceHue) -> vello::peniko::Color {
    static PALETTE: std::sync::LazyLock<crate::Theme> =
        std::sync::LazyLock::new(crate::Theme::dark);
    PALETTE.tokens.colors.presence.hue(hue)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Nothing,
    Pill,
    CatchUp,
}

pub struct ChangeMark {
    key: String,
    shape: Shape,
    element: Element,
}

impl ChangeMark {
    fn build(key: &str, mut props: ChangeMarkProps) -> Self {
        let catch_up = props.catch_up(key);
        let (shape, element) = match catch_up {
            Some(catch_up) => (Shape::CatchUp, catch_up),
            None => match props.chip(key) {
                Some(chip) => (Shape::Pill, chip),
                None => (
                    Shape::Nothing,
                    Box::new(crate::widget::arrange::Stack::stack()) as Element,
                ),
            },
        };
        Self {
            key: key.to_owned(),
            shape,
            element,
        }
    }

    /// What `props` would show, as a shape.
    fn shape_of(props: &ChangeMarkProps) -> Shape {
        if props.window && !props.content.is_empty() {
            Shape::CatchUp
        } else if props.by.is_some() {
            Shape::Pill
        } else {
            Shape::Nothing
        }
    }
}

impl crate::widget::Decorator for ChangeMark {
    type Inner = dyn crate::widget::Widget;

    fn inner(&self) -> &(dyn crate::widget::Widget + 'static) {
        self.element.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn crate::widget::Widget + 'static) {
        self.element.as_mut()
    }

    fn name(&self) -> &'static str {
        "ChangeMark"
    }
}

impl component::Described for ChangeMark {
    type Props = ChangeMarkProps;

    fn from_props(key: &str, props: ChangeMarkProps) -> Self {
        Self::build(key, props)
    }

    /// Who changed what, what they did and what pressing it does change where the mark stands, so the
    /// catch-up it opened stays open.
    fn set_props(&mut self, props: ChangeMarkProps) -> bool {
        if Self::shape_of(&props) != self.shape {
            return false;
        }
        let Some(pill) = props.pill(&self.key) else {
            return true;
        };
        match self.shape {
            Shape::Nothing => true,
            Shape::Pill => {
                let Some(button) = self
                    .element
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<Button>())
                else {
                    return false;
                };
                button.take_words(&pill)
            }
            Shape::CatchUp => {
                let Some(pair) = self
                    .element
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<crate::widget::PopoverButton<Container>>())
                else {
                    return false;
                };
                pair.button_mut().take_words(&pill)
            }
        }
    }

    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn component::SlotViews> {
        if slot != "content" || self.shape != Shape::CatchUp {
            return None;
        }
        let pair = self
            .element
            .as_any_mut()?
            .downcast_mut::<crate::widget::PopoverButton<Container>>()?;
        let column = pair
            .popover_mut()
            .content_mut()
            .child_mut()?
            .as_any_mut()?
            .downcast_mut::<crate::widget::arrange::Arranged<crate::widget::arrange::FlexLayout>>(
            )?;
        Some(column)
    }
}

#[cfg(test)]
mod tests {
    /// A window's mark names one change in the singular and several in the type's plural: "Mia changed
    /// one message", never "changed one messages".
    #[test]
    fn a_mark_names_one_change_in_the_singular() {
        crate::i18n::I18nManager::register_widget_messages();
        crate::i18n::I18nManager::set_locale("en-US");
        let said = |count: usize| {
            crate::i18n::widget_format(
                "change-window-changed",
                &[
                    ("who", "Mia".into()),
                    ("count", count.into()),
                    ("noun", "messages".into()),
                    ("one", "message".into()),
                ],
            )
        };
        assert_eq!(said(1), "Mia changed one message");
        assert_eq!(said(3), "Mia changed 3 messages");
    }

    /// A window's mark of changes that only put items in says what is new and who put it there: "6 new
    /// messages from Mia, Leo and Ada", never that they changed them.
    #[test]
    fn a_mark_of_new_items_says_what_is_new() {
        crate::i18n::I18nManager::register_widget_messages();
        crate::i18n::I18nManager::set_locale("en-US");
        let said = |count: i64| {
            crate::i18n::widget_format(
                "change-window-new",
                &[
                    ("who", "Mia, Leo and Ada".into()),
                    ("count", count.into()),
                    ("noun", "messages".into()),
                    ("one", "message".into()),
                ],
            )
        };
        assert_eq!(said(1), "One new message from Mia, Leo and Ada");
        assert_eq!(said(6), "6 new messages from Mia, Leo and Ada");
    }

    /// German's perfect tense agrees with the person it names ("Mia hat …"), and a window's mark names
    /// everyone who changed it ("Leo und Mia"), so its sentence names them without a verb after the
    /// names: it reads right for one person and for several.
    #[test]
    fn a_german_mark_reads_right_for_several_names() {
        let language = crate::i18n::speak_window_language(&[(
            "de-DE".to_owned(),
            "change-mark-probe = Probe".to_owned(),
        )]);
        assert_eq!(language, "de-DE");
        let said = |who: &str, count: usize, more: bool| {
            crate::i18n::widget_format(
                if more {
                    "change-window-changed-more"
                } else {
                    "change-window-changed"
                },
                &[
                    ("who", who.into()),
                    ("count", count.into()),
                    ("noun", "Kommentare".into()),
                    ("one", "Kommentar".into()),
                ],
            )
        };
        assert_eq!(said("Mia", 1, false), "Geändert von Mia: 1 Kommentar");
        assert_eq!(
            said("Leo und Mia", 5, false),
            "Geändert von Leo und Mia: 5 Kommentare"
        );
        assert_eq!(
            said("Leo und Mia", 5, true),
            "Geändert von Leo und Mia: mindestens 5 Kommentare"
        );
    }
}

//! `ChangeMark`: what someone else changed that the person has not seen yet
//! (task-legibility §3), as one pill in the changer's colour: their face, and the task run it
//! was part of. Activating it shows what they did.
//!
//! The UI host places one on every list row, table row and window whose resource has unseen
//! changes, with no code of the app's; a view that wants it elsewhere places `ui.ChangeMark(r)`
//! itself and the host fills in who and which run, and places none of its own for `r`.

use crate::component::{self, Component, Event, Person, ResourceId};
use crate::tokens::colors::PresenceHue;
use crate::widget::{Chip, ChipStyle, Element, Tone};

/// What someone else changed of a resource that the person looking has not seen yet: who, in
/// which task run, and whether they made, changed or removed it. The host fills all but `of`
/// from the resource's versions; activating it shows what they did.
#[derive(Component, Default)]
#[component(name = "ChangeMark", role = "Button", label = "run")]
pub struct ChangeMarkProps {
    /// The resource whose changes it marks.
    #[component(positional)]
    pub of: Option<ResourceId>,
    /// Who made the newest change the person has not seen.
    pub by: Option<Person>,
    /// Everyone who made one of the changes the person has not seen, newest first, each once.
    pub by_all: Vec<Person>,
    /// What the newest change did, in the words history says it with ("moved this to").
    pub said: Option<String>,
    /// What one item of it is called, plural ("tasks"): the noun a window's count names.
    pub noun: Option<String>,
    /// The task run it was part of, as its task is titled.
    pub run: Option<String>,
    /// What they did: `created`, `changed` or `removed`.
    #[component(data)]
    pub kind: Option<String>,
    /// How many changes of it the person has not seen.
    pub count: i64,
    /// It marks a window, in its header: it says how many changes and by whom.
    pub window: bool,
    /// The window's document holds more resources than the mark counted: what it says is a
    /// lower bound, and says so.
    pub more: bool,
    /// Activated: show what they did, at the run that did it.
    #[component(event)]
    pub activate: Event,
}

/// What a row shows of a change mark, at its own height ([`crate::widget::ListRow::change`]):
/// who (identity and name), what they did said under the row's title, and what pressing their
/// face does.
pub struct RowMark {
    pub identity: String,
    pub name: String,
    pub spoken: String,
    /// What the newest change did, one line under the row's title, "and N more" where there
    /// are more; empty shows no line.
    pub said: String,
    pub show: Option<Box<dyn FnMut()>>,
}

impl ChangeMarkProps {
    /// The same mark again, its activation shared: a row is built anew each time it scrolls
    /// into view.
    #[must_use]
    pub fn again(&self) -> Self {
        Self {
            of: self.of.clone(),
            by: self.by.clone(),
            by_all: self.by_all.clone(),
            said: self.said.clone(),
            noun: self.noun.clone(),
            run: self.run.clone(),
            kind: self.kind.clone(),
            count: self.count,
            window: self.window,
            more: self.more,
            activate: self.activate.clone(),
        }
    }

    /// The mark as a row carries it: the face of who changed it and their colour under the
    /// row, and one line under the row's title saying what they did; who and the run said on
    /// hover and to assistive technology. `None` while nothing is unseen.
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
        // What they did, under the row's title: the sentence history says of the change, after
        // who did it, and how many more of the row's changes wait behind it.
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

    /// The mark beside a row that is no list row (a view of its own): the face of who changed
    /// it alone, as a round pill that keeps the row's height; who and the run on hover.
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

    /// The header's mark: the faces of everyone who changed what the window shows, and a
    /// label naming them and saying how many. `None` while nothing is unseen (no `by`).
    #[must_use]
    pub fn chip(self, key: &str) -> Option<Element> {
        let Self {
            by,
            by_all,
            noun,
            count,
            window,
            more,
            activate,
            ..
        } = self;
        let by = by?;
        let mut who: Vec<Person> = Vec::new();
        for person in by_all.into_iter().chain(std::iter::once(by)) {
            if !who.iter().any(|seen| seen.identity == person.identity) {
                who.push(person);
            }
        }
        // A row's mark shows one face; a window's shows everyone, its label naming the first
        // few and its count saying what they changed.
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
                    if more {
                        "change-window-changed-more"
                    } else {
                        "change-window-changed"
                    },
                    &[
                        ("who", names.into()),
                        ("count", count.into()),
                        ("noun", noun.into()),
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
        let mut button = crate::widget::Button::new(key, &label)
            .role(crate::widget::ButtonRole::Plain)
            .with_faces_of(faces)
            .with_accessible_name(&label)
            .tooltip(label.clone());
        button = match activate {
            Event::None => button,
            Event::Action(binding) => button.on_action(binding),
            Event::View(intent) => button.on_view(intent.sender()),
        };
        Some(Box::new(button))
    }
}

/// What `people` are called, as a sentence names them: "Bo", "Bo and Scout", "Bo, Scout and
/// Kit", and past three "Bo, Scout and 3 others" — the faces count the rest.
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

/// The described entry of `ChangeMark`: the pill, built anew when its props change.
#[must_use]
pub fn entry() -> component::Entry {
    fn build(source: &mut dyn component::Source) -> Result<Element, component::Error> {
        let key = source.key().to_owned();
        let props = ChangeMarkProps::from_source(source)?;
        Ok(match props.chip(&key) {
            Some(chip) => chip,
            // Nothing unseen: nothing to show.
            None => Box::new(crate::widget::arrange::Stack::stack()),
        })
    }
    fn update(_: &mut Element, _: &mut dyn component::Source) -> Result<bool, component::Error> {
        Ok(false)
    }
    fn slot_views<'a>(_: &'a mut Element, _: &str) -> Option<&'a mut dyn component::SlotViews> {
        None
    }
    component::Entry {
        name: ChangeMarkProps::NAME,
        describe: ChangeMarkProps::describe,
        build,
        update,
        slot_views,
    }
}

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
    /// The task run it was part of, as its task is titled.
    pub run: Option<String>,
    /// What they did: `created`, `changed` or `removed`.
    pub kind: Option<String>,
    /// How many changes of it the person has not seen.
    pub count: i64,
    /// It marks a window, in its header: it says how many changes and by whom.
    pub window: bool,
    /// Activated: show what they did, at the run that did it.
    #[component(event)]
    pub activate: Event,
}

/// What a row shows of a change mark, at its own height ([`crate::widget::ListRow::change`]):
/// who (identity and name), what is said of it, and what pressing their face does.
pub struct RowMark {
    pub identity: String,
    pub name: String,
    pub spoken: String,
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
            run: self.run.clone(),
            kind: self.kind.clone(),
            count: self.count,
            window: self.window,
            activate: self.activate.clone(),
        }
    }

    /// The mark as a row carries it: the face of who changed it and their colour under the
    /// row; who and the run said on hover and to assistive technology. `None` while nothing is
    /// unseen.
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
        Some(RowMark {
            identity: by.identity,
            name: by.name,
            spoken,
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

    /// The pill: who and the run, in their colour, led by their face. `None` while nothing is
    /// unseen (no `by`).
    #[must_use]
    pub fn chip(self, key: &str) -> Option<Chip> {
        let Self {
            by,
            run,
            kind,
            count,
            window,
            activate,
            ..
        } = self;
        let by = by?;
        let who = by.name.clone();
        let label = match run.filter(|run| !run.is_empty()) {
            _ if window => crate::i18n::widget_format(
                "change-window",
                &[("count", count.max(1).into()), ("who", who.clone().into())],
            ),
            Some(run) => crate::i18n::widget_format(
                "change-mark-run",
                &[("who", who.clone().into()), ("run", run.into())],
            ),
            None => crate::i18n::widget_format("change-mark-who", &[("who", who.clone().into())]),
        };
        let did = match kind.as_deref() {
            Some("created") => "change-mark-created",
            Some("removed") => "change-mark-removed",
            _ => "change-mark-changed",
        };
        let hue = PresenceHue::of_identity(&by.identity);
        let chip = Chip::new(label)
            .with_id(key.to_owned())
            .with_face(who.clone(), face_colour(hue))
            .style(ChipStyle::Tinted(Tone::Presence(hue)))
            .tooltip(crate::i18n::widget_format(
                did,
                &[("who", who.clone().into())],
            ))
            .with_accessible_name(crate::i18n::widget_format(
                "change-mark-open",
                &[("who", who.into())],
            ));
        Some(match activate {
            Event::None => chip,
            Event::Action(binding) => chip.on_action(binding),
            Event::View(intent) => chip.on_view(intent.sender()),
        })
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
            Some(chip) => Box::new(chip),
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

//! Who wrote an item and when, and where it came from: the fields a page
//! shows beside its title, each its own element. The author is a person chip,
//! an item moved here names the tool it came from as a link to its page
//! there, and the day is its own date — never one sentence joined with
//! separators, and never the whole thing as text a reader would have to parse.

use ui_core::peniko::Color;

use tilo_resource::{MemberId, Origin};
use ui_core::i18n::ArgValue;
use ui_core::tokens::{PresenceHue, Rhythm, TextRole};
use ui_core::widget::{Chip, Container, Element, Link, Row, Text};

use super::people::{Actor, People};

/// The tool an item was first written in, and the page it has there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// The tool's name (`Forgejo`, `Linear`, `Outline`).
    pub name: String,
    /// How the tool names the item (`tilo#5602`, `ENG-12`), when it has a
    /// name of its own.
    pub reference: Option<String>,
    /// Its page there: what the source element opens, when the tool gives a
    /// page.
    pub url: Option<String>,
}

impl Source {
    /// The source as a person reads it: the tool, and the name the item had
    /// there when that name is one a person could say. A reference that is
    /// only machine ids (a UUID an importer copied out of another tool) names
    /// nothing a reader knows; the tool alone says it.
    #[must_use]
    pub fn label(&self) -> String {
        match self.readable_reference() {
            Some(reference) => crate::copy_catalogue::t_args(
                "provenance-source-named",
                &[
                    ("source", self.name.clone().into()),
                    ("reference", reference.to_owned().into()),
                ],
            ),
            None => self.name.clone(),
        }
    }

    /// The name it had there, when a reader would recognize it.
    fn readable_reference(&self) -> Option<&str> {
        let reference = self.reference.as_deref()?.trim();
        (!reference.is_empty() && !is_uuid(reference)).then_some(reference)
    }
}

/// Whether `text` is a bare UUID: the 8-4-4-4-12 hex form tools number their
/// own items by. It identifies an item to a machine, not to a person.
fn is_uuid(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(at, byte)| match at {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

/// Who wrote an item and when, in the fields a view renders one by one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    /// Its author here, when it was written here; none for an item moved here
    /// from elsewhere, whose author is a name, not a member.
    pub actor: Option<Actor>,
    /// The name its author is shown by: the workspace's name for them here,
    /// their name where it was written.
    pub name: String,
    /// The colour the author's face wears.
    pub hue: PresenceHue,
    /// Where it came from, for an item moved here.
    pub source: Option<Source>,
    /// When it shows it was written, in milliseconds since the epoch.
    pub at_ms: i64,
}

impl Provenance {
    /// The day it was written, as a person reads a date; nothing when the
    /// origin names no day (it has none, or none this build keeps).
    #[must_use]
    pub fn date(&self) -> String {
        if self.at_ms <= 0 {
            return String::new();
        }
        jiff::Timestamp::from_millisecond(self.at_ms)
            .map(|at| {
                let day = at.to_zoned(jiff::tz::TimeZone::UTC).date();
                ui_core::format::Formats::current().date(day)
            })
            .unwrap_or_default()
    }

    /// The day as the row draws it. A moved item's day is the day it was
    /// written in the tool it came from, not the day it moved here: it says
    /// which of the two days beside it is which, "Written Oct 1, 2026" where
    /// the item's own history here says it was created today.
    #[must_use]
    pub fn date_label(&self) -> String {
        let date = self.date();
        // A day the origin does not name shows nothing rather than "Written".
        if date.is_empty() {
            return date;
        }
        match &self.source {
            Some(_) => {
                crate::copy_catalogue::t_args("provenance-written-on", &[("date", date.into())])
            }
            None => date,
        }
    }

    /// One description of all of it, for a reader: built from the same
    /// fields the row shows, so what is spoken and what is drawn never say
    /// different things.
    #[must_use]
    pub fn spoken(&self) -> String {
        // A reader hears what the chip's mark says to the eye: that an agent wrote it.
        let name: ArgValue = match &self.actor {
            Some(actor) if actor.agent() => {
                ui_core::history_source::agent_spoken_label(&self.name).into()
            }
            _ => self.name.clone().into(),
        };
        let date: ArgValue = self.date().into();
        let Some(source) = &self.source else {
            return crate::copy_catalogue::t_args(
                "provenance-written-here",
                &[("name", name), ("date", date)],
            );
        };
        let tool: ArgValue = source.name.clone().into();
        match source.readable_reference() {
            Some(reference) => crate::copy_catalogue::t_args(
                "provenance-written-there-named",
                &[
                    ("name", name),
                    ("source", tool),
                    ("reference", reference.to_owned().into()),
                    ("date", date),
                ],
            ),
            None => crate::copy_catalogue::t_args(
                "provenance-written-there",
                &[("name", name), ("source", tool), ("date", date)],
            ),
        }
    }

    /// The row a page shows: the author's chip, the source it came from as a
    /// link to the original, and the day — separate elements, one group for a
    /// reader.
    #[must_use]
    pub fn row(&self, id: &str) -> Element {
        let (red, green, blue) = ui_core::presence::identity_rgb(self.hue);
        let colour = Color::from_rgb8(red, green, blue);
        let who = self.actor.as_ref().map_or_else(
            || {
                Actor {
                    member: None,
                    name: self.name.clone(),
                    hue: self.hue,
                }
                .person()
            },
            Actor::person,
        );
        let chip = Chip::new(self.name.clone())
            .with_id(format!("{id}-author"))
            .with_face(who, colour);
        let mut row = Row::new()
            .no_wrap()
            .with_rhythm(Rhythm::Related)
            .push(Box::new(chip));
        if let Some(source) = &self.source {
            let label = source.label();
            row = row.push(match &source.url {
                Some(url) => Box::new(
                    Link::new(url)
                        .with_label(&label)
                        .with_text_role(TextRole::Caption)
                        .with_id(&format!("{id}-source")),
                ),
                None => Box::new(
                    Text::new(&label)
                        .with_id(format!("{id}-source"))
                        .with_role(TextRole::Caption)
                        .dimmed(),
                ),
            });
        }
        row = row.push(Box::new(
            Text::new(&self.date_label())
                .with_id(format!("{id}-date"))
                .with_role(TextRole::Caption)
                .dimmed(),
        ));
        Box::new(
            Container::new()
                .with_no_padding()
                .landmark(self.spoken(), format!("{id}-written"))
                .with_child(Box::new(row)),
        )
    }
}

impl Provenance {
    /// An item moved here from elsewhere: by its original author, in the tool
    /// it came from, on the day it was written there (else `at_ms`).
    #[must_use]
    pub fn moved(origin: &Origin, at_ms: i64) -> Self {
        Self {
            actor: None,
            name: origin.author.clone(),
            hue: PresenceHue::of_identity(&origin.author),
            source: Some(Source {
                name: origin.source.clone(),
                reference: origin.reference.clone(),
                url: origin.url.clone(),
            }),
            at_ms: origin.created_ms.unwrap_or(at_ms),
        }
    }
}

impl People {
    /// Who wrote an item and when, in the fields a page shows separately: an
    /// item moved here is by its original author, in the tool it came from,
    /// on the day it was written there; any other by its author here, named
    /// as [`People::actor_kept`] names them, on the day it was written here.
    /// The record's own history keeps who moved it.
    #[must_use]
    pub fn provenance(
        &self,
        author: Option<&MemberId>,
        kept: &str,
        at_ms: i64,
        origin: Option<&Origin>,
    ) -> Provenance {
        if let Some(origin) = origin {
            return Provenance::moved(origin, at_ms);
        }
        let actor = self.actor_kept(author, kept);
        Provenance {
            hue: actor.hue,
            name: actor.name.clone(),
            actor: Some(actor),
            source: None,
            at_ms,
        }
    }
}

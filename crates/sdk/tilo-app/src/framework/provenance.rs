//! Who wrote an item and when, and where it came from: the fields a page
//! shows beside its title, each its own element. The author is a person chip,
//! an item moved here names the tool it came from as a link to its page
//! there, and the day is its own date — never one sentence joined with
//! separators, and never the whole thing as text a reader would have to parse.

use ui_core::peniko::Color;

use tilo_resource::Origin;
use ui_core::i18n::ArgValue;
use ui_core::tokens::{PresenceHue, Rhythm, TextRole};
use ui_core::widget::{Chip, ChipLead, Container, Element, Link, Row, Text};

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
    /// there.
    #[must_use]
    pub fn label(&self) -> String {
        match &self.reference {
            Some(reference) if !reference.trim().is_empty() => crate::copy_catalogue::t_args(
                "provenance-source-named",
                &[
                    ("source", self.name.clone().into()),
                    ("reference", reference.clone().into()),
                ],
            ),
            _ => self.name.clone(),
        }
    }
}

/// Who wrote an item and when, in the fields a view renders one by one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    /// Its author here, when it was written here; none for an item moved here
    /// from elsewhere, whose author is a name, not a participant.
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
    /// The day it was written, as a person reads a date.
    #[must_use]
    pub fn date(&self) -> String {
        let days = i32::try_from(self.at_ms.div_euclid(86_400_000)).unwrap_or_default();
        ui_core::format::NaiveDate::from_num_days_from_ce_opt(days.saturating_add(719_163))
            .map(|day| ui_core::format::Formats::current().date(day))
            .unwrap_or_default()
    }

    /// One description of all of it, for a reader: built from the same
    /// fields the row shows, so what is spoken and what is drawn never say
    /// different things.
    #[must_use]
    pub fn spoken(&self) -> String {
        let name: ArgValue = self.name.clone().into();
        let date: ArgValue = self.date().into();
        let Some(source) = &self.source else {
            return crate::copy_catalogue::t_args(
                "provenance-written-here",
                &[("name", name), ("date", date)],
            );
        };
        let tool: ArgValue = source.name.clone().into();
        let reference = source
            .reference
            .as_deref()
            .filter(|reference| !reference.trim().is_empty());
        match reference {
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
        let mut chip = Chip::new(self.name.clone()).with_id(format!("{id}-author"));
        chip = match &self.actor {
            Some(actor) if actor.agent => chip.with_lead(ChipLead::Agent(colour)),
            _ => chip.with_face(self.name.clone(), colour),
        };
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
            Text::new(&self.date())
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

impl People {
    /// Who wrote an item and when, in the fields a page shows separately: an
    /// item moved here is by its original author, in the tool it came from,
    /// on the day it was written there; any other by its author here, named
    /// as [`People::actor_kept`] names them, on the day it was written here.
    /// The record's own history keeps who moved it.
    #[must_use]
    pub fn provenance(
        &self,
        author: &str,
        kept: &str,
        at_ms: i64,
        origin: Option<&Origin>,
    ) -> Provenance {
        if let Some(origin) = origin {
            let at_ms = origin.created_ms.unwrap_or(at_ms);
            return Provenance {
                actor: None,
                name: origin.author.clone(),
                hue: PresenceHue::of_identity(&origin.author),
                source: Some(Source {
                    name: origin.source.clone(),
                    reference: origin.reference.clone(),
                    url: origin.url.clone(),
                }),
                at_ms,
            };
        }
        let actor = self.actor_kept(author, kept);
        Provenance {
            hue: self.hue(&actor.id),
            name: actor.name.clone(),
            actor: Some(actor),
            source: None,
            at_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ui_core::accesskit::Role as AkRole;
    use ui_core::widget::Widget;

    use super::People;

    #[test]
    fn a_moved_item_names_its_author_its_source_and_its_day_as_separate_fields() {
        let people = People::named(Some("ada"), &[("ada", "Ada")]);
        let origin = tilo_resource::Origin {
            author: "xemmerling".into(),
            handle: Some("xemmerling".into()),
            source: "Forgejo".into(),
            url: Some("https://code.tilo.so/tilo/tilo/issues/5602".into()),
            reference: Some("tilo#5602".into()),
            created_ms: Some(1_763_164_800_000),
            updated_ms: None,
        };
        let moved = people.provenance("agent:grant-a8a42b74", "", 1_900_000_000_000, Some(&origin));
        assert_eq!(moved.name, "xemmerling", "the author is its own field");
        assert!(moved.actor.is_none(), "a moved author is not a participant");
        let source = moved.source.as_ref().expect("the tool it came from");
        assert_eq!(source.name, "Forgejo");
        assert_eq!(source.label(), "Forgejo (tilo#5602)");
        assert_eq!(
            source.url.as_deref(),
            Some("https://code.tilo.so/tilo/tilo/issues/5602"),
            "the source links to the original"
        );
        assert_eq!(
            moved.at_ms, 1_763_164_800_000,
            "the day it was written there"
        );
        let spoken = moved.spoken();
        assert!(
            spoken.contains("xemmerling") && spoken.contains("Forgejo") && spoken.contains("2025"),
            "one description is built from the fields: {spoken}"
        );
        assert!(
            !spoken.contains("tilo#5602") || spoken.contains("(tilo#5602)"),
            "the name it had there reads as its own field: {spoken}"
        );

        let here = people.provenance("ada", "", 1_763_164_800_000, None);
        assert_eq!(here.name, "Ada");
        assert!(here.actor.is_some(), "written here, it is by a participant");
        assert!(here.source.is_none(), "written here, it came from nowhere");
    }

    /// The row a page draws, walked as the accessibility tree a reader meets:
    /// one group named by the whole description, and the same fields under it
    /// as their own elements.
    fn nodes(row: &mut dyn Widget) -> Vec<(ui_core::accesskit::NodeId, ui_core::accesskit::Node)> {
        let theme = ui_core::theme::Theme::dark();
        let mut taffy = crate::advanced::TaffyTree::<()>::new();
        let mut node_map: HashMap<crate::advanced::NodeId, *mut dyn Widget> = HashMap::new();
        row.build_layout(&mut taffy, &theme, &mut node_map);
        ui_core::accesskit_bridge::build_tree_update(&*row, &node_map, None, 1.0).nodes
    }

    #[test]
    fn the_provenance_row_is_the_same_fields_as_the_description_a_reader_hears() {
        let people = People::named(Some("ada"), &[("ada", "Ada")]);
        let origin = tilo_resource::Origin {
            author: "xemmerling".into(),
            handle: None,
            source: "Forgejo".into(),
            url: Some("https://code.tilo.so/tilo/tilo/issues/5602".into()),
            reference: Some("tilo#5602".into()),
            created_ms: Some(1_763_164_800_000),
            updated_ms: None,
        };
        let provenance = people.provenance("agent:grant-a8a42b74", "", 0, Some(&origin));
        let mut row = provenance.row("tasks-issue-written");
        let nodes = nodes(&mut *row);
        let found = |role: AkRole, label: Option<&str>| {
            nodes.iter().any(|(_, node)| {
                node.role() == role && label.is_none_or(|label| node.label() == Some(label))
            })
        };
        assert!(
            found(AkRole::Group, Some(&provenance.spoken())),
            "one group is named by the description built from the fields: {:?}",
            nodes
                .iter()
                .map(|(_, node)| (node.role(), node.label(), node.value()))
                .collect::<Vec<_>>()
        );
        assert!(
            found(AkRole::Link, Some("Forgejo (tilo#5602)")),
            "the source is its own element, linked to the original"
        );
        assert!(
            nodes.iter().any(|(_, node)| node.role() == AkRole::Label
                && node.value() == Some(&provenance.date())),
            "the day is its own element"
        );
        assert!(
            nodes.iter().any(|(_, node)| node.role() == AkRole::Label
                && node.value() == Some("xemmerling")),
            "the author is a person chip"
        );
    }
}

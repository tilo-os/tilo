//! The items device readings list — this computer's Windows programs, containers, drives,
//! printers, journal records and processes — each a row of its own, found whether or not the
//! app that shows them runs.

use tilo_actions::ReadingRow;
use ui::search::system::{ResourceKind, rank_fields};
use ui::text_match::FieldWeight;

use crate::model::{Hit, InvokeTarget, LaunchPlace, Source, Target};

/// The source's id, as the person names it in search's own sources.
pub const DEVICE_ROWS_SOURCE: &str = "device-rows";

/// The most rows one query offers.
const MAX_ROWS: usize = 32;

/// The rows this device's readings publish, in the workspace in front: what a chosen row runs,
/// or the app whose window shows the item when its reading offers no action on it.
pub struct DeviceRows {
    workspace: String,
    rows: Vec<(String, ReadingRow)>,
}

impl DeviceRows {
    /// The rows `rows`, each by the resource whose reading lists it, in `workspace`.
    #[must_use]
    pub fn new(workspace: impl Into<String>, rows: Vec<(String, ReadingRow)>) -> Self {
        Self {
            workspace: workspace.into(),
            rows,
        }
    }

    /// What choosing `row` of `resource` does: the reading's action on the item, named by the
    /// arguments the row carries, else the app that shows it, opened at the item's page.
    fn target(&self, resource: &str, row: &ReadingRow) -> Target {
        if let Some(action) = &row.action {
            let mut target = InvokeTarget::action(
                crate::model::DESKTOP_APP,
                &self.workspace,
                resource,
                action,
            );
            target
                .arguments
                .extend(row.arguments.iter().map(|(name, value)| {
                    (name.clone(), value.clone())
                }));
            return Target::Invoke(Box::new(target));
        }
        let Some(app) = row.app.clone() else {
            return Target::Launch {
                app: String::new(),
                place: LaunchPlace::Focus,
            };
        };
        Target::Invoke(Box::new(InvokeTarget::open(
            app.clone(),
            self.workspace.as_str(),
            format!("tl:{app}"),
            format!("{resource}/{}", row.id),
        )))
    }
}

impl Source for DeviceRows {
    fn id(&self) -> &'static str {
        DEVICE_ROWS_SOURCE
    }

    fn name(&self) -> String {
        crate::i18n::t("search-source-device-readings")
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        let needle = needle.trim();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<Hit> = self
            .rows
            .iter()
            .filter_map(|(resource, row)| {
                let fields: Vec<(&str, FieldWeight)> = std::iter::once((
                    row.title.as_str(),
                    FieldWeight::TITLE,
                ))
                .chain(
                    row.subtitle
                        .as_deref()
                        .map(|subtitle| (subtitle, FieldWeight::SUBTITLE)),
                )
                .chain(row.fields.iter().map(|field| (field.as_str(), FieldWeight::BODY)))
                .collect();
                let score = rank_fields(needle, &fields)?;
                Some(Hit {
                    row: None,
                    preview: None,
                    faces: Vec::new(),
                    outcome: None,
                    kind: row
                        .group
                        .as_deref()
                        .map_or(ResourceKind::Record, |group| {
                            ResourceKind::Custom(group.to_owned())
                        }),
                    provider: self.id().into(),
                    id: format!("{resource}/{}", row.id),
                    title: row.title.clone(),
                    subtitle: row.subtitle.clone(),
                    facts: row.facts.clone(),
                    score,
                    icon: row.icon.as_deref().and_then(ui::icons::Icon::from_name),
                    target: self.target(resource, row),
                })
            })
            .collect();
        hits.sort_by_key(|hit| std::cmp::Reverse(hit.score));
        hits.truncate(MAX_ROWS);
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OPEN_ANCHOR;

    fn partition() -> (String, ReadingRow) {
        (
            "disks".to_owned(),
            ReadingRow::new("sda2", "Data")
                .subtitled("ext4")
                .grouped("Disks")
                .opened_by("mount", [("device", "sda2")]),
        )
    }

    fn container() -> (String, ReadingRow) {
        (
            "containers".to_owned(),
            ReadingRow::new("c1", "build-cache")
                .subtitled("running")
                .field("postgres:17")
                .shown_by("so.tilo.containers"),
        )
    }

    /// A partition is found by its label and mounted by the action its reading offers, and a
    /// container the reading offers no action on opens the app that shows it, at its page.
    #[test]
    fn a_reading_s_items_are_found_and_opened() {
        let rows = DeviceRows::new("workspace", vec![partition(), container()]);

        let found = rows.query("data");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "Data");
        assert_eq!(found[0].subtitle.as_deref(), Some("ext4"));
        let Target::Invoke(target) = &found[0].target else {
            panic!("a partition is mounted by the reading's action");
        };
        assert_eq!(
            target.address,
            tilo_actions::ActionAddress::new("disks", "mount")
        );
        assert_eq!(target.arguments["device"], "sda2");

        let found = rows.query("postgres");
        assert_eq!(found.len(), 1, "a container is found by its image");
        let Target::Invoke(target) = &found[0].target else {
            panic!("a container opens the app showing it");
        };
        assert_eq!(target.app, "so.tilo.containers");
        assert_eq!(target.address.resource, "tl:so.tilo.containers");
        assert_eq!(target.arguments[OPEN_ANCHOR], "containers/c1");

        assert!(rows.query("zebra").is_empty());
    }
}

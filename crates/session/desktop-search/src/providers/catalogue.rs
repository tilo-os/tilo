//! Apps a person can get, from the catalogue search shares with the Store
//! ([`app_catalogue`]): Tilo apps the registries offer, Flathub apps from
//! Flatpak's cached metadata, and every Tilo app's documents and offered
//! actions, whether or not the app is on this device.
//!
//! - A **Tilo app** has no install step: choosing it opens it, and the host
//!   downloads it first when it is absent.
//! - A **Flathub app** is never installed by choosing it: its row opens its
//!   Store page, where the person decides. One built only for another
//!   processor is found all the same and says so.
//! - A Tilo app's **document** ("Workbook") opens the app, which starts on a
//!   new one; its **action** ("Show tasks") runs by address through the
//!   session's action host, which gets and starts the app first.
//!
//! A Tilo app's rows rank just below installed apps of equal match; a
//! Flathub app's Store page after everything the person has. The host hands
//! in only what is offered ([`app_catalogue::Catalogue::offered`]): nothing
//! the device has, nothing blocked.

use app_catalogue::{CatalogueEntry, Source as Origin};
use ui::search::FieldWeight;
use ui::search::system::{MAX_PROVIDER_SCORE, ResourceKind};

use crate::model::{Hit, InvokeTarget, LaunchPlace, Source, Target};

/// How far below an installed app of the same match a Tilo app to get
/// ranks.
const CATALOGUE_STEP: i32 = 3;
/// What a Flathub app's Store page gives way at the merge: after everything
/// the person already has.
const STORE_PAGE_INTENT: f32 = -2.0;
/// The most rows the catalogue offers one query. The Store is something to
/// find, not a page of the card: the nearest few are what a person decides
/// from, and the rest of the search is what they already have.
const MAX_ROWS: usize = 6;

/// What the palette groups an app the person does not have under: the Store,
/// beside the apps they do have. One query then finds what is here first, and
/// the catalogue's many offers stay one group of their own.
fn store_kind() -> ResourceKind {
    ResourceKind::Custom(crate::i18n::t("search-kind-store"))
}

/// The Store's page of app `id`, where it can be reviewed, blocked or
/// installed by hand; `None` for an id no Store link may carry.
#[must_use]
pub fn store_page(id: &str) -> Option<String> {
    let valid = !id.is_empty()
        && id.len() <= 255
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'));
    valid.then(|| format!("{}{id}", crate::model::STORE_PAGE_PREFIX))
}

/// The glyph the app's own rows wear: its own by name, else its
/// category's, so a row offering an app looks like that app.
fn glyph(entry: &CatalogueEntry) -> ui::icons::Icon {
    ui::icons::app_glyph(&entry.id, entry.category.as_deref())
}

enum Row {
    App(usize),
    Document(usize, usize),
    Action(usize, usize),
}

pub struct StoreCatalogue {
    /// The workspace an app's action runs in.
    workspace: String,
    entries: Vec<CatalogueEntry>,
    rows: Vec<Row>,
    fields: Vec<Vec<(String, FieldWeight)>>,
    ranker: ui::search::ListIndex,
}

impl StoreCatalogue {
    /// Search `offered`, running an app's actions in `workspace`.
    #[must_use]
    pub fn new(offered: Vec<CatalogueEntry>, workspace: impl Into<String>) -> Self {
        let mut rows = Vec::new();
        let mut fields = Vec::new();
        for (at, entry) in offered.iter().enumerate() {
            let mut found = vec![(entry.name.clone(), FieldWeight::TITLE)];
            found.extend(
                entry
                    .other_names
                    .iter()
                    .chain(&entry.keywords)
                    .map(|word| (word.clone(), FieldWeight::KEYWORD)),
            );
            found.push((entry.id.clone(), FieldWeight::IDENTITY));
            fields.push(found);
            rows.push(Row::App(at));
            if entry.source.needs_install() {
                continue;
            }
            for (document, kind) in entry.resource_types.iter().enumerate() {
                // The kind of document names the row; the app it belongs to
                // is what the row says under it, not another name for it —
                // else every document an app offers answers to the app's own
                // name.
                fields.push(vec![(kind.label.clone(), FieldWeight::TITLE)]);
                rows.push(Row::Document(at, document));
            }
            for (action, offer) in entry.actions.iter().enumerate() {
                fields.push(vec![
                    (offer.label.clone(), FieldWeight::TITLE),
                    (entry.name.clone(), FieldWeight::SUBTITLE),
                ]);
                rows.push(Row::Action(at, action));
            }
        }
        Self {
            workspace: workspace.into(),
            entries: offered,
            rows,
            fields,
            ranker: ui::search::ListIndex::default()
                .with_ranks(ui::search::system::application_ranks),
        }
    }

    fn hit(&self, row: &Row, score: i32) -> Option<Hit> {
        let score = score
            .min(MAX_PROVIDER_SCORE)
            .saturating_sub(CATALOGUE_STEP)
            .max(1);
        let (id, title, subtitle, facts, kind, outcome, icon, target) = match *row {
            Row::App(at) => {
                let entry = self.entries.get(at)?;
                let (facts, outcome, target) = match &entry.source {
                    Origin::Tilo { registry } => (
                        vec![crate::i18n::t_args(
                            "search-catalogue-from",
                            &[("registry", registry.as_str().into())],
                        )],
                        None,
                        Target::Launch {
                            app: entry.id.clone(),
                            place: LaunchPlace::Focus,
                        },
                    ),
                    Origin::Flathub { .. } => {
                        let mut facts = vec![crate::i18n::t("search-catalogue-flathub")];
                        if entry.not_built_for_this_device {
                            facts.push(crate::i18n::t("search-catalogue-not-for-device"));
                        }
                        (
                            facts,
                            Some(crate::i18n::t("search-catalogue-open-store")),
                            Target::Open {
                                uri: store_page(&entry.id)?,
                                fallback: None,
                            },
                        )
                    }
                };
                (
                    format!("app:{}", entry.id),
                    entry.name.clone(),
                    (!entry.tagline.is_empty()).then(|| entry.tagline.clone()),
                    facts,
                    store_kind(),
                    outcome,
                    Some(glyph(entry)),
                    target,
                )
            }
            Row::Document(at, document) => {
                let entry = self.entries.get(at)?;
                let kind = entry.resource_types.get(document)?;
                (
                    format!("document:{}:{}", entry.id, kind.kind),
                    kind.label.clone(),
                    Some(entry.name.clone()),
                    Vec::new(),
                    store_kind(),
                    None,
                    Some(glyph(entry)),
                    Target::Launch {
                        app: entry.id.clone(),
                        place: LaunchPlace::Focus,
                    },
                )
            }
            Row::Action(at, action) => {
                let entry = self.entries.get(at)?;
                let offer = entry.actions.get(action)?;
                (
                    format!("action:{}:{}:{}", entry.id, offer.resource, offer.action),
                    offer.label.clone(),
                    offer
                        .description
                        .clone()
                        .or_else(|| Some(entry.name.clone())),
                    Vec::new(),
                    // A row offering an action belongs to the Store, with
                    // the app it is offered with: the person has neither
                    // yet, and the palette lists the two together.
                    store_kind(),
                    None,
                    Some(glyph(entry)),
                    Target::Invoke(Box::new(InvokeTarget::action(
                        &entry.id,
                        &self.workspace,
                        &offer.resource,
                        &offer.action,
                    ))),
                )
            }
        };
        Some(Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome,
            kind,
            provider: self.id().into(),
            id,
            title,
            subtitle,
            facts,
            score,
            // The app's own glyph, from its category: a row offering an app
            // looks like all of that app's own rows do, not like everything
            // else on offer.
            icon,
            target,
        })
    }
}

impl Source for StoreCatalogue {
    fn id(&self) -> &'static str {
        crate::model::CATALOGUE_SOURCE
    }

    fn name(&self) -> String {
        crate::i18n::t("search-source-store")
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        if needle.trim().is_empty() {
            return Vec::new();
        }
        let ranks = crate::rank::rank_rows(&self.ranker, needle, &self.fields);
        let mut found: Vec<Hit> = self
            .rows
            .iter()
            .zip(ranks)
            .filter_map(|(row, rank)| self.hit(row, rank?))
            .collect();
        found.sort_by_key(|hit| std::cmp::Reverse(hit.score));
        found.truncate(MAX_ROWS);
        found
    }

    fn intent(&self, hit: &Hit) -> f32 {
        if matches!(hit.target, Target::Open { .. }) {
            STORE_PAGE_INTENT
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_catalogue::entry::{CatalogueAction, Installation, ResourceType};

    fn entry(id: &str, name: &str, source: Origin) -> CatalogueEntry {
        CatalogueEntry {
            id: id.into(),
            source,
            name: name.into(),
            other_names: Vec::new(),
            tagline: String::new(),
            description: String::new(),
            keywords: Vec::new(),
            category: None,
            icon_file: None,
            icon_url: None,
            resource_types: Vec::new(),
            actions: Vec::new(),
            not_built_for_this_device: false,
        }
    }

    fn tilo() -> Origin {
        Origin::Tilo {
            registry: "tilo".into(),
        }
    }

    fn flathub() -> Origin {
        Origin::Flathub {
            remote: "flathub".into(),
            installation: Installation::User,
        }
    }

    #[test]
    fn a_tilo_app_to_get_opens_and_its_documents_and_actions_come_with_it() {
        let mut sheets = entry("so.tilo.sheets", "Sheets", tilo());
        sheets.resource_types = vec![ResourceType {
            kind: "sheets.workbook".into(),
            label: "Workbook".into(),
        }];
        let mut calendar = entry("so.tilo.calendar", "Calendar", tilo());
        calendar.actions = vec![CatalogueAction {
            resource: "calendar.calendar".into(),
            action: "show-tasks".into(),
            label: "Show tasks".into(),
            description: None,
        }];
        let catalogue = StoreCatalogue::new(vec![sheets, calendar], "workspace");
        let app = &catalogue.query("sheets")[0];
        assert_eq!(
            app.target,
            Target::Launch {
                app: "so.tilo.sheets".into(),
                place: LaunchPlace::Focus,
            }
        );
        assert!(app.score < MAX_PROVIDER_SCORE, "below an installed app");
        assert_eq!(
            app.kind,
            store_kind(),
            "an app to get is the Store's, not one the person has"
        );
        assert_eq!(
            app.icon,
            Some(ui::icons::app_glyph("so.tilo.sheets", None)),
            "the row wears the app's own glyph"
        );
        let workbook = catalogue.query("workbook");
        assert_eq!(workbook[0].subtitle.as_deref(), Some("Sheets"));
        assert_eq!(workbook[0].kind, store_kind(), "nor is its document");
        let tasks = catalogue.query("show tasks");
        let Target::Invoke(target) = &tasks[0].target else {
            panic!("an offered action runs by address");
        };
        assert_eq!(target.app, "so.tilo.calendar");
        assert_eq!(target.workspace, "workspace");
        assert_eq!(
            target.address,
            tilo_actions::ActionAddress::new("calendar.calendar", "show-tasks")
        );
        assert_eq!(
            tasks[0].kind,
            store_kind(),
            "an offered action is the Store's too: the person has neither it \
             nor the app it runs in"
        );
    }

    #[test]
    fn the_catalogue_offers_the_nearest_rows_of_a_wide_query() {
        let mut tasks = entry("so.tilo.tasks", "Tasks", tilo());
        let action = |at: usize, label: &str| CatalogueAction {
            resource: "tasks.task".into(),
            action: format!("add-{at}"),
            label: label.into(),
            description: None,
        };
        tasks.actions = (0..MAX_ROWS + 4)
            // Every one of these matches the query's word as one of its own:
            // nothing tells them apart but the order the catalogue offers
            // them in.
            .map(|at| action(at, &format!("Add task {at}")))
            // The nearest comes last: the row the query names is nearer than
            // the rows that only say the word, so a catalogue that kept the
            // rows that came first would leave it out.
            .chain([action(MAX_ROWS + 4, "Task")])
            .collect();
        let catalogue = StoreCatalogue::new(vec![tasks], "workspace");
        let hits = catalogue.query("task");
        assert_eq!(hits.len(), MAX_ROWS, "the card is not the Store");
        let titles: Vec<&str> = hits.iter().map(|hit| hit.title.as_str()).collect();
        assert!(
            titles.contains(&"Task"),
            "the rows kept are the nearest, not whichever came first: {titles:?}"
        );
        let mut scores: Vec<i32> = hits.iter().map(|hit| hit.score).collect();
        scores.sort_unstable_by(|a, b| b.cmp(a));
        assert!(
            scores.first() > scores.last(),
            "the rows are cut from the nearest down: {scores:?}"
        );
    }

    #[test]
    fn a_flathub_app_opens_its_store_page_and_never_installs() {
        let mut steam = entry("com.valvesoftware.Steam", "Steam", flathub());
        steam.not_built_for_this_device = true;
        let catalogue = StoreCatalogue::new(vec![steam], "workspace");
        let hits = catalogue.query("steam");
        assert_eq!(
            hits[0].target,
            Target::Open {
                uri: "tilo-store:app/com.valvesoftware.Steam".into(),
                fallback: None
            }
        );
        assert_eq!(
            hits[0].facts,
            [
                crate::i18n::t("search-catalogue-flathub"),
                crate::i18n::t("search-catalogue-not-for-device")
            ],
            "where it comes from, and that it is not for this device"
        );
        assert_eq!(
            hits[0].outcome.as_deref(),
            Some(crate::i18n::t("search-catalogue-open-store").as_str()),
            "choosing it opens its Store page, it does not install"
        );
        assert_eq!(
            hits[0].icon,
            Some(ui::icons::app_glyph("com.valvesoftware.Steam", None)),
            "the row wears the app's glyph, whoever offers it"
        );
        assert!(catalogue.intent(&hits[0]) < 0.0);
        assert_eq!(store_page("bad id"), None);
    }
}

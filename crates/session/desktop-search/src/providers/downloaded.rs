//! The programs the person downloaded and added to their apps, and the
//! programs running now that have no entry yet (the registry's offers).
//!
//! The rows are read from the registry's one reading
//! ([`downloaded::DownloadedApps`]), not from the machine's desktop entries:
//! a downloaded app's row says what it is (an `AppImage`, a program), starts
//! it the way every host app starts, and, when its file went away, says so
//! and asks where the file is now instead of starting nothing. An offer is
//! the row to add that program to the apps, which is the only way back to
//! the question after the person closed it.
//!
//! Every row acts by address on the session's published registry: adding,
//! locating and everything else go through the same actions an agent or
//! another person's device invokes. An app's own row comes with the rows
//! that act on it — uninstalling it, which asks first, and taking it off the
//! apps — so what the person sees of an app in search is everything they can
//! do with it.

use std::collections::BTreeMap;

use tilo_actions::remote::downloaded_apps::{self as wire, DownloadedApp, DownloadedKind};
use tilo_actions::{ActionAddress, ActionDescriptor};
use ui::icons::Icon;
use ui::search::FieldWeight;
use ui::search::system::ResourceKind;

use crate::i18n::{t, t_args};
use crate::model::{DESKTOP_APP, Hit, InvokeTarget, Source, Target};
use crate::rank;

/// The source id of the downloaded programs.
pub const DOWNLOADED_SOURCE: &str = "downloaded-programs";

/// The session's resource of the questions about the person's apps, and the
/// action that asks whether to uninstall one: the same question the app
/// list asks, so the answer is the registry's own action.
pub const APPS_RESOURCE: &str = "session:apps";
pub const ASK_UNINSTALL: &str = "uninstall";
/// The parameter naming the app [`ASK_UNINSTALL`] is about: its entry's id,
/// or the name the person knows it by.
pub const APP_PARAM: &str = "app";

/// How far below its app's row an action on it ranks: the app is what a
/// person typing its name finds first, and what they can do with it under
/// it.
const ACTION_STEP: i32 = 1;

/// The registry's reading, and the app rows' name for an app whose file is
/// gone: the action that asks where it is now, as the session publishes it.
pub struct Downloaded {
    room: String,
    reading: wire::DownloadedApps,
    /// Each row's searchable text, and the row itself without its score.
    fields: Vec<Vec<(String, FieldWeight)>>,
    hits: Vec<Hit>,
    /// How far below its rank each row ranks ([`ACTION_STEP`] for an action
    /// on an app, nothing for the app).
    steps: Vec<i32>,
    ranker: ui::search::ListIndex,
}

/// What kind of download an app is, in the desktop's language.
fn kind_label(kind: DownloadedKind) -> String {
    match kind {
        DownloadedKind::AppImage => t("search-downloaded-appimage"),
        DownloadedKind::Program => t("search-downloaded-program"),
    }
}

impl Downloaded {
    /// The rows of the registry's reading ([`wire::DownloadedApps`]), asked
    /// for in `room`, with the actions `published` of it (the session's own
    /// published surface): what a row whose file is gone runs.
    #[must_use]
    pub fn new(
        room: impl Into<String>,
        reading: wire::DownloadedApps,
        published: &[ActionDescriptor],
    ) -> Self {
        let room = room.into();
        let locate: BTreeMap<String, ActionDescriptor> = published
            .iter()
            .filter(|action| action.address.action == wire::LOCATE)
            .map(|action| (action.address.resource.as_str().to_owned(), action.clone()))
            .collect();
        let mut fields = Vec::new();
        let mut hits = Vec::new();
        let mut steps = Vec::new();
        for app in &reading.apps {
            let (field, hit) = app_row(&room, app, &locate);
            fields.push(field);
            hits.push(hit);
            steps.push(0);
            for (field, hit) in app_action_rows(&room, app) {
                fields.push(field);
                hits.push(hit);
                steps.push(ACTION_STEP);
            }
        }
        for offer in &reading.offers {
            let (field, hit) = offer_row(&room, offer);
            fields.push(field);
            hits.push(hit);
            steps.push(0);
        }
        Self {
            room,
            reading,
            fields,
            hits,
            steps,
            ranker: ui::search::ListIndex::default()
                .with_ranks(ui::search::system::application_ranks),
        }
    }

    /// The registry's reading these rows were built from.
    #[must_use]
    pub const fn reading(&self) -> &wire::DownloadedApps {
        &self.reading
    }

    /// The room the rows act in.
    #[must_use]
    pub fn room(&self) -> &str {
        &self.room
    }

    /// The app rows: what the person added to their apps.
    fn apps(&self) -> impl Iterator<Item = &Hit> {
        self.hits
            .iter()
            .filter(|hit| hit.kind == ResourceKind::Application)
    }
}

/// One app's searchable text and row: it starts the app, or, when its file
/// is gone, says so and asks where the file is now.
fn app_row(
    room: &str,
    app: &DownloadedApp,
    locate: &BTreeMap<String, ActionDescriptor>,
) -> (Vec<(String, FieldWeight)>, Hit) {
    let kind = kind_label(app.kind);
    // What the row says under the app's name: what happened to its file,
    // when it is gone, else what the app says of itself.
    let subtitle = if app.present {
        app.comment.clone()
    } else {
        Some(t("search-downloaded-missing"))
    };
    let target = if app.present {
        Target::Launch {
            app: app.launch.clone(),
        }
    } else {
        let resource = wire::app(&app.desktop_id);
        let address = ActionAddress::new(resource.as_str(), wire::LOCATE);
        let descriptor = locate.get(resource.as_str()).cloned().map(|mut action| {
            // The row names the app whose file the person is looking for,
            // not the action alone: several may say that at once.
            action.action_label = t_args(
                "search-downloaded-locate",
                &[("name", app.name.clone().into())],
            );
            action
        });
        Target::Invoke(Box::new(InvokeTarget {
            app: DESKTOP_APP.into(),
            room: room.to_owned(),
            address,
            arguments: serde_json::Map::new(),
            descriptor,
        }))
    };
    let field = vec![
        (app.name.clone(), FieldWeight::TITLE),
        (app.desktop_id.clone(), FieldWeight::IDENTITY),
        (kind.clone(), FieldWeight::KEYWORD),
        (subtitle.clone().unwrap_or_default(), FieldWeight::SUBTITLE),
    ];
    let hit = Hit {
        faces: Vec::new(),
        outcome: None,
        kind: ResourceKind::Application,
        provider: DOWNLOADED_SOURCE.into(),
        id: app.desktop_id.clone(),
        title: app.name.clone(),
        subtitle,
        facts: vec![kind],
        score: 0,
        icon: None,
        target,
    };
    (field, hit)
}

/// The rows that act on one app of the person's, under the app's own row:
/// uninstalling it, which asks them on the uninstall sheet first, the one
/// question every door asks, and taking it off the apps while its file
/// stays.
fn app_action_rows(room: &str, app: &DownloadedApp) -> [(Vec<(String, FieldWeight)>, Hit); 2] {
    // Uninstalling goes to the session, whose uninstall sheet holds the
    // question; the answer is the registry's own action, as from the sheet
    // the app list opens.
    let mut ask = serde_json::Map::new();
    ask.insert(
        APP_PARAM.to_owned(),
        serde_json::Value::String(app.desktop_id.clone()),
    );
    let uninstall = Target::Invoke(Box::new(InvokeTarget {
        app: DESKTOP_APP.into(),
        room: room.to_owned(),
        address: ActionAddress::new(APPS_RESOURCE, ASK_UNINSTALL),
        arguments: ask,
        descriptor: None,
    }));
    // Taking it off the apps moves nothing and asks nothing, so its row is
    // the registry's own action.
    let remove = Target::Invoke(Box::new(InvokeTarget {
        app: DESKTOP_APP.into(),
        room: room.to_owned(),
        address: ActionAddress::new(wire::app(&app.desktop_id), wire::REMOVE),
        arguments: serde_json::Map::new(),
        descriptor: None,
    }));
    [
        action_row(
            app,
            "uninstall:",
            Icon::Trash2,
            t_args(
                "search-downloaded-uninstall",
                &[("name", app.name.clone().into())],
            ),
            uninstall,
        ),
        action_row(
            app,
            "remove-from-apps:",
            Icon::X,
            t_args(
                "search-downloaded-remove",
                &[("name", app.name.clone().into())],
            ),
            remove,
        ),
    ]
}

/// One action on `app` as a row: titled what it does to the app, found by
/// the app's name as the app's own row is, and under the id prefix (`id`)
/// that tells it from the app and from the other action.
fn action_row(
    app: &DownloadedApp,
    id: &str,
    icon: Icon,
    label: String,
    target: Target,
) -> (Vec<(String, FieldWeight)>, Hit) {
    let kind = kind_label(app.kind);
    let field = vec![
        (app.name.clone(), FieldWeight::TITLE),
        (label.clone(), FieldWeight::TITLE),
        (app.desktop_id.clone(), FieldWeight::IDENTITY),
        (kind.clone(), FieldWeight::KEYWORD),
    ];
    let hit = Hit {
        faces: Vec::new(),
        outcome: None,
        kind: ResourceKind::Action,
        provider: DOWNLOADED_SOURCE.into(),
        id: format!("{id}{}", app.desktop_id),
        title: label,
        subtitle: None,
        facts: vec![kind],
        score: 0,
        icon: Some(icon),
        target,
    };
    (field, hit)
}

/// One offer's searchable text and row: adding that program to the person's
/// apps, with the file it runs from.
fn offer_row(room: &str, offer: &wire::DownloadedOffer) -> (Vec<(String, FieldWeight)>, Hit) {
    let kind = kind_label(offer.kind);
    let title = t_args(
        "search-downloaded-add",
        &[("name", offer.name.clone().into())],
    );
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        wire::FILE_PARAM.into(),
        serde_json::Value::String(offer.file.clone()),
    );
    let field = vec![
        (offer.name.clone(), FieldWeight::TITLE),
        (title.clone(), FieldWeight::TITLE),
        (offer.file.clone(), FieldWeight::IDENTITY),
        (kind.clone(), FieldWeight::KEYWORD),
    ];
    let hit = Hit {
        faces: Vec::new(),
        outcome: None,
        kind: ResourceKind::Action,
        provider: DOWNLOADED_SOURCE.into(),
        id: format!("add:{}", offer.file),
        title,
        subtitle: None,
        facts: vec![kind],
        score: 0,
        icon: None,
        target: Target::Invoke(Box::new(InvokeTarget {
            app: DESKTOP_APP.into(),
            room: room.to_owned(),
            address: ActionAddress::new(wire::RESOURCE, wire::ADD),
            arguments,
            descriptor: None,
        })),
    };
    (field, hit)
}

impl Source for Downloaded {
    fn id(&self) -> &'static str {
        DOWNLOADED_SOURCE
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        if needle.trim().is_empty() {
            // Nothing typed lists the apps, as the launcher lists every app;
            // an offer is a question the person is asked, not a row they
            // find by browsing.
            return self
                .apps()
                .map(|hit| {
                    let mut hit = hit.clone();
                    hit.score = ui::search::system::starting_point_rank(&hit.id);
                    hit
                })
                .collect();
        }
        let ranks = rank::rank_rows(&self.ranker, needle, &self.fields);
        self.hits
            .iter()
            .zip(ranks)
            .zip(&self.steps)
            .filter_map(|((hit, score), step)| {
                let mut hit = hit.clone();
                hit.score = score?.saturating_sub(*step).max(1);
                Some(hit)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading() -> wire::DownloadedApps {
        wire::DownloadedApps {
            apps: vec![
                DownloadedApp {
                    desktop_id: "tilo-downloaded-Dropped".into(),
                    name: "Dropped Tool".into(),
                    kind: DownloadedKind::AppImage,
                    file: "/home/ana/Applications/Dropped.AppImage".into(),
                    copies: Vec::new(),
                    present: true,
                    artwork: Some(
                        "/home/ana/.local/share/tilo/downloaded-apps/icons/dropped.png".into(),
                    ),
                    comment: Some("Unpacks and runs".into()),
                    launch: "host:tilo-downloaded-Dropped".into(),
                },
                DownloadedApp {
                    desktop_id: "tilo-downloaded-Gone".into(),
                    name: "Gone Tool".into(),
                    kind: DownloadedKind::Program,
                    file: "/home/ana/Downloads/Gone".into(),
                    copies: Vec::new(),
                    present: false,
                    artwork: None,
                    comment: None,
                    launch: "host:tilo-downloaded-Gone".into(),
                },
            ],
            offers: vec![wire::DownloadedOffer {
                file: "/home/ana/Downloads/Waiting.AppImage".into(),
                name: "Waiting".into(),
                kind: DownloadedKind::AppImage,
                artwork: None,
                declined: false,
            }],
            failures: Vec::new(),
        }
    }

    fn locate() -> Vec<ActionDescriptor> {
        tilo_actions::door::survey(&[tilo_actions::ResourceSpec::new(
            wire::app("tilo-downloaded-Gone"),
            "so.tilo.downloaded-app",
            "Gone Tool",
        )
        .with_action(tilo_actions::ActionSpec::immediate_encoded(
            wire::LOCATE,
            "Locate",
            tilo_actions::ExecutionLocus::ClientLocal,
            tilo_actions::EncodedAction {
                type_id: "so.tilo.downloaded-app".into(),
                payload: Vec::new(),
            },
        ))])
    }

    #[test]
    fn an_app_is_found_by_its_name_and_starts_through_the_host() {
        let downloaded = Downloaded::new("room", reading(), &locate());
        let hits = downloaded.query("dropped");
        assert_eq!(hits.len(), 3, "the app, and what can be done with it");
        assert_eq!(hits[0].title, "Dropped Tool");
        assert_eq!(hits[0].kind, ResourceKind::Application);
        assert_eq!(hits[0].facts, ["AppImage"]);
        assert_eq!(
            hits[0].target,
            Target::Launch {
                app: "host:tilo-downloaded-Dropped".into()
            }
        );
        assert!(
            hits[1..].iter().all(|hit| hit.score < hits[0].score),
            "the app comes before the actions on it"
        );
        assert_eq!(downloaded.query("").len(), 2, "browsing lists the apps");
    }

    /// The app's row comes with the two rows that act on it: uninstalling it,
    /// which asks the session first, and taking it off the apps, which is the
    /// registry's action.
    #[test]
    fn an_app_offers_uninstalling_it_and_taking_it_off_the_apps() {
        let downloaded = Downloaded::new("room", reading(), &locate());
        let hits = downloaded.query("dropped");
        let titles: Vec<&str> = hits.iter().map(|hit| hit.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Dropped Tool",
                "Uninstall Dropped Tool",
                "Remove Dropped Tool from apps"
            ]
        );
        let Target::Invoke(uninstall) = &hits[1].target else {
            panic!("uninstalling is asked first");
        };
        assert_eq!(
            uninstall.address,
            ActionAddress::new(APPS_RESOURCE, ASK_UNINSTALL)
        );
        assert_eq!(uninstall.arguments[APP_PARAM], "tilo-downloaded-Dropped");
        assert!(uninstall.descriptor.is_none());
        let Target::Invoke(remove) = &hits[2].target else {
            panic!("taking it off the apps is the registry's action");
        };
        assert_eq!(
            remove.address,
            ActionAddress::new(wire::app("tilo-downloaded-Dropped"), wire::REMOVE)
        );
        assert!(remove.arguments.is_empty());
    }

    #[test]
    fn an_offer_is_the_row_to_add_that_program() {
        let downloaded = Downloaded::new("room", reading(), &locate());
        let hits = downloaded.query("waiting");
        assert_eq!(hits.len(), 1, "the app it would become is not an app yet");
        assert_eq!(hits[0].title, "Add Waiting to apps");
        let Target::Invoke(target) = &hits[0].target else {
            panic!("adding an app is an action");
        };
        assert_eq!(
            target.address,
            ActionAddress::new(wire::RESOURCE, wire::ADD)
        );
        assert_eq!(
            target.arguments[wire::FILE_PARAM],
            "/home/ana/Downloads/Waiting.AppImage"
        );
    }

    #[test]
    fn an_app_whose_file_is_gone_says_so_and_asks_where_it_is() {
        let downloaded = Downloaded::new("room", reading(), &locate());
        let hits = downloaded.query("gone");
        assert_eq!(hits[0].subtitle.as_deref(), Some("Can't find this app"));
        assert_eq!(hits[0].facts, ["Program"]);
        let Target::Invoke(target) = &hits[0].target else {
            panic!("finding the file is an action");
        };
        assert_eq!(
            target.address,
            ActionAddress::new(wire::app("tilo-downloaded-Gone").as_str(), wire::LOCATE)
        );
        let descriptor = target
            .descriptor
            .as_ref()
            .expect("the row asks for the file");
        assert_eq!(descriptor.action_label, "Locate Gone Tool");
        assert_eq!(
            hits[1].title, "Uninstall Gone Tool",
            "an app whose file went away still leaves the person's apps"
        );
    }
}

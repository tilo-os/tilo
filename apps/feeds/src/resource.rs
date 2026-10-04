//! Feeds as a [`tilo_resource::Resource`]: one action vocabulary, one
//! publication of the library, and the reader as its surface.
//!
//! Document mutations (`new-folder`, `add-subscription`, per-folder / per-feed /
//! per-article ops) live on this resource via [`Resource::declarations`] /
//! [`Resource::bound_actions`] / [`Resource::children`]. Refreshing and OPML
//! import/export change nothing by themselves: they are the reader's
//! [`FeedsEffect`]s, whose results reach the library as ordinary actions.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, ResourceApp, ResourceSurface, SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_actions::{
    ActionParam, ActionSpec, PROMPTED_PARAM, ParamType, Resource as PublishedResource,
};
use ui::tilo_resource;
use ui::tilo_resource::{ActionDecl, Doc, Effect, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;
use uuid::Uuid;

use crate::actions::{
    FEEDS_ACTION_TYPE, FeedsAction, add_folder_action, add_subscription_action, mark_all_read_spec,
    publish_children, register, resolve_child, resource_ctx_from_library,
};
use crate::app::FeedsApp;
use crate::doc::FeedsDoc;
use crate::model::{Filter, ReadFont, ReadSettings, ReadTheme, Store};

/// Stable id for the published reader root.
pub const READER_RESOURCE_ID: &str = "feeds.reader";

/// Shared library projection: subscriptions plus durable reading settings.
#[derive(Clone, Debug, Default)]
pub struct FeedsState {
    pub store: Store,
    pub settings: ReadSettings,
}

/// The feeds reader resource.
pub struct Feeds;

impl Resource for Feeds {
    const KIND: &'static str = "feeds.reader";
    const LABEL: &'static str = "Feeds";
    const ACTION_TYPE: &'static str = FEEDS_ACTION_TYPE;

    type State = FeedsState;
    type Action = FeedsAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<FeedsState, ModelError> {
        Ok(FeedsState {
            store: FeedsDoc::load(doc),
            settings: FeedsDoc::read_settings(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "add-subscription",
                "Add subscription",
                vec![ActionParam::required(
                    PROMPTED_PARAM,
                    ParamType::Text,
                    "Feed URL",
                )],
                |args, _| {
                    let url = args.text(PROMPTED_PARAM).unwrap_or_default();
                    Ok(add_subscription_action(Uuid::new_v4(), url, None, 0))
                },
            )
            .describe("Subscribe to a feed URL."),
            ActionDecl::new(
                "new-folder",
                "New folder",
                vec![ActionParam::required(
                    PROMPTED_PARAM,
                    ParamType::Text,
                    "Name",
                )],
                |args, _| {
                    let name = args.text(PROMPTED_PARAM).unwrap_or_default();
                    Ok(add_folder_action(Uuid::new_v4(), name, 0))
                },
            )
            .describe("Create a subscription folder."),
        ]
    }

    fn bound_actions(state: &FeedsState) -> Vec<ActionSpec> {
        // Folder, feed and article actions ride [`Self::children`].
        vec![mark_all_read_spec(&state.store.unread_ids_for(Filter::All))]
    }

    fn children(state: &FeedsState) -> Vec<PublishedResource> {
        publish_children(&resource_ctx_from_library(&state.store, state.settings))
    }

    fn child(state: &FeedsState, id: &str) -> Option<PublishedResource> {
        resolve_child(&state.store, state.settings, id)
    }

    /// Every subscription, article and tag, each opening where it is shown.
    /// An article whose subscription is gone cannot be opened, so it is not
    /// found either.
    fn search(state: &FeedsState) -> Vec<SearchEntry> {
        let store = &state.store;
        let feeds = store.feeds.iter().map(|feed| {
            SearchEntry::new(
                feed.title.clone(),
                format!("{}\n{}", feed.url, feed.site_url),
            )
            .anchored(crate::actions::feed_resource_id(feed.id))
            .invoking(crate::actions::open_feed_address(feed.id))
        });
        let articles = store.articles.iter().filter_map(|article| {
            let feed = store.feed(article.feed_id)?;
            Some(
                SearchEntry::new(
                    article.title.clone(),
                    [
                        article.author.as_str(),
                        article.summary.as_str(),
                        feed.title.as_str(),
                        &article.tags.join(" "),
                    ]
                    .join("\n"),
                )
                .anchored(crate::actions::article_resource_id(article.id))
                .invoking(crate::actions::open_article_address(article.id)),
            )
        });
        let tags = store.tags().into_iter().map(|tag| {
            let id = crate::actions::tag_resource_id(&tag);
            SearchEntry::new(format!("#{tag}"), "Tagged articles")
                .invoking(ui::tilo_actions::ActionAddress::new(id.clone(), "open"))
                .anchored(id)
        });
        feeds.chain(articles).chain(tags).collect()
    }
}

/// What the reader does that changes no document by itself; what it brings
/// in reaches the library as ordinary actions.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "feeds.effects")]
pub enum FeedsEffect {
    /// Refresh all subscriptions.
    #[effect(job)]
    RefreshAll,
    /// Import OPML…
    ImportOpml,
    /// Export OPML…
    #[effect(permission = "read")]
    ExportOpml,
}

/// The reader's messages, in every host that mounts it.
static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[
        ("en-US", include_str!("../locales/en-US.ftl")),
        ("de-DE", include_str!("../locales/de-DE.ftl")),
    ],
    "feeds-open-link-invalid",
)
.with_system_locale();

/// Framework app.
pub struct FeedsShell;

/// Scenarios that show the reader in a narrow, phone-sized window.
const NARROW_SCENARIOS: [&str; 2] = ["sidebar-open", "detail"];

impl ResourceApp for FeedsShell {
    type Resource = Feeds;
    const ID: &'static str = "so.tilo.feeds";
    const TITLE: &'static str = "Feeds";
    const ICON: Option<&'static str> = Some("feeds");

    fn view(cx: &mut ViewCx<Feeds>) -> Element {
        cx.surface(FeedsApp::new())
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "sidebar-open",
            "detail",
            "empty",
            "reader-settings",
            "refresh",
            "long-titles",
            "context-menu",
            "post-import",
            "add-subscription",
            "tags",
        ]
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        if scenario.is_some_and(|name| NARROW_SCENARIOS.contains(&name)) {
            (560, 800)
        } else {
            (1280, 800)
        }
    }

    /// The bundled sample library, subscribed and fetched, under every
    /// scenario but the first-run `empty` one.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Feeds>> {
        if name == Some("empty") {
            return Vec::new();
        }
        let mut library = crate::sample::store();
        let mut actions = vec![
            FeedsAction::ImportSubscriptions {
                folders: library.folders.clone(),
                feeds: library.feeds.clone(),
            },
            FeedsAction::UpsertFetched {
                articles: library.articles.clone(),
                feeds: Vec::new(),
            },
        ];
        match name.unwrap_or_default() {
            "reader-settings" => {
                actions.push(crate::actions::set_read_settings_action(ReadSettings {
                    font: ReadFont::Serif,
                    theme: ReadTheme::Sepia,
                    ..ReadSettings::default()
                }));
            }
            // Titles long enough that the sidebar elides them to one line
            // with a fade instead of wrapping into the next row.
            "long-titles" => actions.extend(
                library
                    .feeds
                    .iter()
                    .zip([
                        "International Space Station Daily Operations & Mission Updates",
                        "The Exhaustively Comprehensive Front-End Engineering Weekly Digest",
                    ])
                    .map(|(feed, title)| crate::actions::rename_feed_action(feed.id, title)),
            ),
            // The sidebar right after an OPML import folded new folders and
            // feeds in.
            "post-import" => {
                let had_feeds: Vec<_> = library.feeds.iter().map(|feed| feed.id).collect();
                let had_folders: Vec<_> = library.folders.iter().map(|folder| folder.id).collect();
                crate::opml::import_into(
                    &mut library,
                    &crate::opml::parse(crate::opml::sample_opml()),
                );
                actions.push(FeedsAction::ImportSubscriptions {
                    folders: library
                        .folders
                        .iter()
                        .filter(|folder| !had_folders.contains(&folder.id))
                        .cloned()
                        .collect(),
                    feeds: library
                        .feeds
                        .iter()
                        .filter(|feed| !had_feeds.contains(&feed.id))
                        .cloned()
                        .collect(),
                });
            }
            _ => {}
        }
        actions
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
    }
}

impl ResourceSurface<Feeds, FeedsEffect> for FeedsApp {
    fn present(&mut self, state: &Rc<FeedsState>) {
        self.present_library(state.store.clone(), state.settings);
    }

    fn attach(&mut self, cx: SurfaceCx<Feeds>) {
        self.attach_dispatcher(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(&mut self, effect: FeedsEffect, cx: &mut EffectCx<Feeds, Self>) -> EffectAnswer {
        match effect {
            FeedsEffect::RefreshAll => {
                if let Some((_, reason)) = self.refresh_unavailable() {
                    return Err(reason.into());
                }
                self.refresh_for(cx.reply());
                Ok(Effected::Pending)
            }
            FeedsEffect::ImportOpml => {
                self.request_import();
                Ok(Effected::done())
            }
            FeedsEffect::ExportOpml => {
                self.export_opml();
                Ok(Effected::done())
            }
        }
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.refresh_unavailable()
            .map(|(code, reason)| EffectStatus::unavailable(FeedsEffect::REFRESH_ALL, code, reason))
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{
        add_subscription_action, remove_subscription_action, rename_feed_action,
        set_read_settings_action,
    };
    use crate::model::{Article, Feed, FeedKind, Folder};
    use ui::framework::testing::TestApp;

    fn apply(action: &FeedsAction) -> Invoke<Feeds> {
        Invoke::apply(action).unwrap()
    }

    /// The reader on the sample library, laid out as the screenshots are.
    fn sample_app() -> TestApp<FeedsShell> {
        let mut app = TestApp::<FeedsShell>::new().unwrap();
        for invoke in FeedsShell::scenario(None) {
            app.invoke(invoke).unwrap();
        }
        app.settle();
        app
    }

    fn article(id: u128, feed_id: Uuid, title: &str) -> Article {
        Article {
            id: Uuid::from_u128(id),
            feed_id,
            title: title.into(),
            url: format!("https://example.test/{id}"),
            summary: String::new(),
            author: String::new(),
            published: 0,
            read: false,
            starred: false,
            tags: Vec::new(),
            blocks: Vec::new(),
        }
    }

    /// Opening the app writes nothing: the default reading settings stand
    /// unwritten and there is no library to show.
    #[test]
    fn opening_writes_nothing() {
        let mut app = TestApp::<FeedsShell>::new().unwrap();
        app.settle();
        assert!(app.state().store.is_empty());
        assert!(!app.read_document(FeedsDoc::has_settings));
        assert!(!app.undo(), "there is nothing to undo");
    }

    /// The reader's keys, the palette and an agent all edit the library
    /// through the same door, one undo step each.
    #[test]
    fn the_reader_edits_through_the_framework_door() {
        let mut app = sample_app();
        let state = app.state();
        let first = state
            .store
            .articles_for(Filter::All)
            .first()
            .map(|article| (article.id, article.starred))
            .unwrap();

        // `s` stars the open article.
        app.harness().key("s");
        app.settle();
        let starred =
            |app: &TestApp<FeedsShell>| app.state().store.article(first.0).unwrap().starred;
        assert_eq!(starred(&app), !first.1);
        assert!(app.undo(), "starring is one undo step");
        assert_eq!(starred(&app), first.1);

        // `a` marks everything read.
        app.harness().key("a");
        app.settle();
        assert_eq!(app.state().store.total_unread(), 0);

        // The palette's prompt subscribes; a blank one adds nothing.
        let before = app.state().store.feeds.len();
        app.call(
            "add-subscription",
            serde_json::json!({ PROMPTED_PARAM: "https://example.test/updates.xml" }),
        )
        .unwrap();
        assert!(
            app.state()
                .store
                .feeds
                .iter()
                .any(|feed| feed.url == "https://example.test/updates.xml")
        );
        app.call(
            "add-subscription",
            serde_json::json!({ PROMPTED_PARAM: "   " }),
        )
        .unwrap();
        assert_eq!(app.state().store.feeds.len(), before + 1);

        // A reading preference written by another window shows here.
        app.invoke(apply(&set_read_settings_action(ReadSettings {
            font: ReadFont::Serif,
            ..ReadSettings::default()
        })))
        .unwrap();
        assert_eq!(app.state().settings.font, ReadFont::Serif);
    }

    /// Unsubscribing the last feed — here, through the palette, or by a
    /// collaborator — leaves the reader showing no rows of it.
    #[test]
    fn unsubscribing_the_last_feed_empties_the_reader() {
        let mut app = TestApp::<FeedsShell>::new().unwrap();
        let feed = Uuid::from_u128(0xFEED_0030);
        app.invoke(apply(&add_subscription_action(
            feed,
            "https://example.test/only.xml",
            None,
            0,
        )))
        .unwrap();
        app.invoke(apply(&rename_feed_action(feed, "The Only Feed")))
            .unwrap();
        app.invoke(apply(&FeedsAction::UpsertFetched {
            articles: vec![article(0xFEED_0031, feed, "The Only Article")],
            feeds: Vec::new(),
        }))
        .unwrap();
        app.settle();
        let shown = app.harness().inspection_json().unwrap();
        assert!(shown.contains("The Only Feed"), "{shown}");
        assert!(shown.contains("The Only Article"), "{shown}");

        app.invoke(apply(&remove_subscription_action(feed)))
            .unwrap();
        app.settle();
        assert!(app.state().store.is_empty());
        let shown = app.harness().inspection_json().unwrap();
        assert!(!shown.contains("The Only Feed"), "{shown}");
        assert!(!shown.contains("The Only Article"), "{shown}");
    }

    /// Refreshing and OPML import/export are the reader's effects: every
    /// caller sees them, and refreshing says why it cannot run without
    /// subscriptions.
    #[test]
    fn refreshing_and_opml_are_effects_of_the_reader() {
        let mut app = TestApp::<FeedsShell>::new().unwrap();
        let surface = app.agent_surface();
        let action = |id: &str| {
            surface
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["resource"] == READER_RESOURCE_ID && row["action"] == id)
                .cloned()
                .unwrap_or_else(|| panic!("{id} is published: {surface:#}"))
        };
        for id in ["refresh-all", "import-opml", "export-opml"] {
            assert_eq!(action(id)["operation"], "Effect", "{id}");
        }
        assert!(
            app.effect(&FeedsEffect::RefreshAll).is_err(),
            "there is nothing to refresh"
        );
    }

    /// What a refresh fetched reaches every collaborator but is nobody's
    /// undo step, while an OPML import is one step.
    #[test]
    fn fetched_rows_are_no_undo_step_and_an_import_is_one() {
        let mut app = TestApp::<FeedsShell>::new().unwrap();
        let feed_id = Uuid::from_u128(0xFEED_0010);
        app.invoke(apply(&add_subscription_action(
            feed_id,
            "https://example.test/feed.xml",
            None,
            0,
        )))
        .unwrap();
        app.invoke(apply(&rename_feed_action(feed_id, "Mine")))
            .unwrap();
        app.invoke(apply(&FeedsAction::UpsertFetched {
            feeds: Vec::new(),
            articles: vec![article(0xFEED_0011, feed_id, "Fetched")],
        }))
        .unwrap();
        assert_eq!(app.state().store.articles.len(), 1);

        // Undo takes back the rename, past the fetched article, which stays.
        assert!(app.undo());
        let state = app.state();
        assert_ne!(state.store.feeds[0].title, "Mine");
        assert_eq!(state.store.articles.len(), 1);

        // The subscriptions an import added go in one step.
        let folder = Folder {
            id: Uuid::from_u128(0xFEED_0020),
            name: "Reading".into(),
            order: 0,
            collapsed: false,
        };
        let imported = |n: u128, url: &str| Feed {
            id: Uuid::from_u128(n),
            title: url.into(),
            url: url.into(),
            site_url: url.into(),
            folder: Some(folder.id),
            order: 0,
            kind: FeedKind::Rss,
        };
        app.invoke(apply(&FeedsAction::ImportSubscriptions {
            folders: vec![folder.clone()],
            feeds: vec![
                imported(0xFEED_0021, "https://a.test/rss"),
                imported(0xFEED_0022, "https://b.test/rss"),
            ],
        }))
        .unwrap();
        assert_eq!(app.state().store.feeds.len(), 3);
        assert_eq!(app.state().store.folders.len(), 1);
        assert!(app.undo());
        assert_eq!(app.state().store.feeds.len(), 1);
        assert!(app.state().store.folders.is_empty());
    }

    /// Every screenshot is the framework's capture: the library comes from
    /// the scenario's actions, the open popovers from the reader's staging,
    /// and the narrow scenarios get a phone-sized window.
    #[test]
    fn captures_stage_through_actions_and_the_reader() {
        let mut detail = TestApp::<FeedsShell>::capture(Some("detail")).unwrap();
        assert_eq!(detail.harness().inspection().viewport, [560, 800]);
        assert!(!detail.state().store.feeds.is_empty());
        assert!(
            detail
                .harness()
                .inspection_json()
                .unwrap()
                .contains("Back to articles")
        );

        let settings = TestApp::<FeedsShell>::capture(Some("reader-settings")).unwrap();
        assert_eq!(settings.state().settings.font, ReadFont::Serif);
        assert_eq!(settings.state().settings.theme, ReadTheme::Sepia);

        let long = TestApp::<FeedsShell>::capture(Some("long-titles")).unwrap();
        assert!(
            long.state()
                .store
                .feeds
                .iter()
                .any(|feed| feed.title.starts_with("International Space Station"))
        );

        let imported = TestApp::<FeedsShell>::capture(Some("post-import")).unwrap();
        assert!(
            imported
                .state()
                .store
                .feeds
                .iter()
                .any(|feed| feed.url == "https://jvns.ca/atom.xml")
        );

        let mut tags = TestApp::<FeedsShell>::capture(Some("tags")).unwrap();
        assert_eq!(tags.harness().inspection().viewport, [1280, 800]);
        assert!(
            tags.harness()
                .inspection_json()
                .unwrap()
                .contains("Article tags")
        );

        let empty = TestApp::<FeedsShell>::capture(Some("empty")).unwrap();
        assert!(empty.state().store.is_empty());
    }

    /// Every host that mounts the reader loads its messages.
    #[test]
    fn the_catalog_resolves_the_link_messages() {
        FeedsShell::catalog().unwrap().ensure();
        for (key, value) in [
            (
                "feeds-open-link-invalid",
                ui::i18n!("feeds-open-link-invalid"),
            ),
            (
                "feeds-open-link-started",
                ui::i18n!("feeds-open-link-started"),
            ),
            (
                "feeds-open-link-failed",
                ui::i18n!("feeds-open-link-failed"),
            ),
        ] {
            assert_ne!(key, value);
            assert!(!value.is_empty());
        }
    }
}

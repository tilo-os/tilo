//! Browser as a [`tilo_resource::Resource`]: bookmarks and preferences are
//! document actions; window navigation is published from
//! [`Resource::local_operations`], and what only this window knows (the
//! page, history, downloads, extensions) from the surface's live rows.

use std::rc::Rc;

use ui::framework::{
    Command, EffectAnswer, EffectCx, Effected, LiveRow, ResourceApp, ResourceSurface, SurfaceCx,
    ViewCx,
};
use ui::tilo_actions::{ActionParam, ParamType};
use ui::tilo_resource::{
    ActionDecl, Doc, Invoke, ModelError, Resource, SearchEntry, declaration_spec,
};
use ui::widget::Element;

use crate::BrowserWindow;
use crate::actions::{
    BROWSER_ACTION_TYPE, BrowserAction, bookmark_resource_id, bookmarks_from_doc, register,
};
use crate::browser_widget::BrowserWidget;
use crate::browsing_mode::BrowsingMode;
use crate::effects::BrowserEffect;
use crate::library::{Bookmark, Bookmarks};
use crate::settings::Settings;

/// Document projection: collaborative bookmarks, the preferences that
/// follow the person to every browser window, and the origins tracker
/// protection is off for.
#[derive(Clone, Debug, Default)]
pub struct BrowserState {
    pub bookmarks: Bookmarks,
    pub preferences: Settings,
    pub site_exceptions: std::collections::BTreeSet<String>,
}

/// The preferences `doc` holds; each one it never set reads its default.
fn preferences_from_doc(doc: &Doc) -> Settings {
    use ui::doc::{read_bool, read_text};
    let root = ui::automerge::ROOT;
    let mut preferences = Settings::default();
    if let Some(enabled) = read_bool(doc, &root, crate::settings::ADBLOCK_PATH) {
        preferences.adblock_enabled = enabled;
    }
    if let Some(enabled) = read_bool(doc, &root, crate::settings::CLEAR_ON_EXIT_PATH) {
        preferences.clear_on_exit = enabled;
    }
    if let Some(url) = read_text(doc, &root, crate::settings::HOMEPAGE_PATH)
        .filter(|url| crate::actions::normalized_bookmark_url(url).is_some())
    {
        preferences.homepage = url;
    }
    if let Some(template) = read_text(doc, &root, crate::settings::SEARCH_ENGINE_PATH)
        .filter(|template| crate::actions::normalized_bookmark_url(template).is_some())
    {
        preferences.search_engine = template;
    }
    preferences
}

/// Browser library resource.
pub struct Browser;

impl Resource for Browser {
    /// Matches the published application root kind.
    const KIND: &'static str = "browser.application";
    const LABEL: &'static str = "Browser";
    const ACTION_TYPE: &'static str = BROWSER_ACTION_TYPE;

    type State = BrowserState;
    type Action = BrowserAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<BrowserState, ModelError> {
        Ok(BrowserState {
            bookmarks: bookmarks_from_doc(doc).unwrap_or_default(),
            preferences: preferences_from_doc(doc),
            site_exceptions: crate::actions::site_exceptions_from_doc(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "set-adblock",
                "Set ad blocking",
                vec![ActionParam::required_owned(
                    "enabled",
                    ParamType::Boolean,
                    "Whether network ad blocking is on",
                )],
                |args, _| {
                    Ok(BrowserAction::SetAdblock {
                        enabled: args.boolean("enabled").unwrap_or(true),
                    })
                },
            ),
            ActionDecl::new(
                "set-clear-on-exit",
                "Set clear-on-exit",
                vec![ActionParam::required_owned(
                    "enabled",
                    ParamType::Boolean,
                    "Whether private data clears when the window closes",
                )],
                |args, _| {
                    Ok(BrowserAction::SetClearOnExit {
                        enabled: args.boolean("enabled").unwrap_or(false),
                    })
                },
            ),
        ]
    }

    fn children(state: &BrowserState) -> Vec<tilo_actions::Resource> {
        let mut bookmarks =
            tilo_actions::Resource::new("browser.bookmarks", "browser.bookmarks", "Bookmarks");
        for entry in state.bookmarks.entries() {
            bookmarks = bookmarks.with_child(bookmark_child(entry));
        }
        vec![bookmarks, settings_child()]
    }

    fn search(state: &BrowserState) -> Vec<SearchEntry> {
        state
            .bookmarks
            .entries()
            .iter()
            .map(|entry| {
                let id = bookmark_resource_id(&entry.url);
                SearchEntry::new(
                    if entry.title.is_empty() {
                        entry.url.clone()
                    } else {
                        entry.title.clone()
                    },
                    entry.url.clone(),
                )
                .invoking(ui::tilo_actions::ActionAddress::new(id.as_str(), "open"))
                .anchored(id)
            })
            .collect()
    }
}

fn bookmark_child(entry: &Bookmark) -> tilo_actions::Resource {
    let title = if entry.title.is_empty() {
        entry.url.clone()
    } else {
        entry.title.clone()
    };
    // Open is the surface's: it navigates this window. Remove is the
    // document action, tombstoning the tags the document holds now.
    let remove = BrowserAction::RemoveBookmark {
        url: entry.url.clone(),
        observed_tags: entry.tags.clone(),
    };
    tilo_actions::Resource::new(bookmark_resource_id(&entry.url), "browser.bookmark", title)
        .with_action(declaration_spec(&ActionDecl::<Browser>::new(
            "remove",
            "Remove bookmark",
            vec![],
            move |_, _| Ok(remove.clone()),
        )))
}

/// The preferences, as the document actions that set them.
fn settings_child() -> tilo_actions::Resource {
    let toggle = |id: &'static str, label: &str, build: fn(bool) -> BrowserAction| {
        declaration_spec(&ActionDecl::<Browser>::new(
            id,
            label,
            vec![ActionParam::required_owned(
                "enabled",
                ParamType::Boolean,
                "Whether the setting is enabled",
            )],
            move |args, _| Ok(build(args.boolean("enabled").unwrap_or(false))),
        ))
    };
    let text = |id: &'static str,
                label: &str,
                parameter: &'static str,
                build: fn(String) -> BrowserAction| {
        declaration_spec(&ActionDecl::<Browser>::new(
            id,
            label,
            vec![ActionParam::required_owned(
                parameter,
                ParamType::Text,
                parameter,
            )],
            move |args, _| Ok(build(args.text(parameter).unwrap_or_default().to_owned())),
        ))
    };
    tilo_actions::Resource::new("browser.settings", "browser.settings", "Settings")
        .with_action(toggle(
            "set-adblock",
            "Set ad and tracker blocking",
            |enabled| BrowserAction::SetAdblock { enabled },
        ))
        .with_action(toggle(
            "set-clear-on-exit",
            "Set clear data on exit",
            |enabled| BrowserAction::SetClearOnExit { enabled },
        ))
        .with_action(text("set-homepage", "Set homepage", "url", |url| {
            BrowserAction::SetHomepage { url }
        }))
        .with_action(text(
            "set-search-engine",
            "Set search engine template",
            "template",
            |template| BrowserAction::SetSearchEngine { template },
        ))
        .with_action(declaration_spec(&ActionDecl::<Browser>::new(
            "set-site-protection",
            "Set tracker protection for a site",
            vec![
                ActionParam::required_owned(
                    "origin",
                    ParamType::Text,
                    "The site's exact origin, such as https://example.org",
                ),
                ActionParam::required_owned(
                    "enabled",
                    ParamType::Boolean,
                    "Whether tracker protection is on for the site",
                ),
            ],
            |args, _| {
                let origin = args.text("origin").unwrap_or_default();
                Ok(BrowserAction::SetSiteProtection {
                    origin: crate::site_controls::origin(origin)
                        .unwrap_or_else(|| origin.to_owned()),
                    enabled: args.boolean("enabled").unwrap_or(true),
                })
            },
        )))
}

/// The screenshot scenarios: the page, the chrome's states, the library's
/// sections and the tab strip.
const SCENARIOS: &[&str] = &[
    "pip-active",
    "pip-empty",
    "settings",
    "history-paused",
    "loading",
    "error",
    "suggestions",
    "library-history",
    "library-bookmarks",
    "library-extensions",
    "library-filtered",
    "downloads-progress",
    "downloads-completed",
    "downloads-error",
    "downloads-empty",
];

/// The page a launch names: one absolute http(s) URL, the only schemes the
/// browser claims. Anything else (a file, `javascript:`, `data:`) is not
/// handed to the engine.
fn launch_target(arguments: &[String]) -> Option<String> {
    arguments
        .iter()
        .find(|arg| {
            arg.split_once(':').is_some_and(|(scheme, rest)| {
                (scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http"))
                    && rest.starts_with("//")
            }) && arg.len() <= 8 * 1024
                && !arg.chars().any(char::is_control)
        })
        .cloned()
}

/// Framework app: the browser chrome is the surface over [`Browser`].
pub struct BrowserShell;

impl ResourceApp for BrowserShell {
    type Resource = Browser;
    const ID: &'static str = "so.tilo.browser";
    const TITLE: &'static str = "Browser";
    const ICON: Option<&'static str> = Some("browser");
    const ARGUMENTS: usize = 1;
    const PRIVATE_WINDOWS: bool = true;

    fn commands() -> &'static [Command] {
        &[
            Command {
                option: "--shots-live",
                run: crate::live_capture::shots_live,
            },
            Command {
                option: "--shots-live-pip",
                run: crate::live_capture::shots_live_pip,
            },
        ]
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            Some("pip-empty" | "suggestions") => (1000, 650),
            Some("settings" | "history-paused") => (900, 700),
            Some("loading" | "error") => (900, 620),
            Some(name) if name.starts_with("library-") || name.starts_with("downloads-") => {
                (1000, 700)
            }
            _ => (1200, 720),
        }
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Browser>> {
        if name != Some("library-bookmarks") {
            return Vec::new();
        }
        [
            ("https://www.rust-lang.org/learn", "Learn Rust", "rust"),
            ("https://developer.mozilla.org/", "MDN Web Docs", "mdn"),
        ]
        .into_iter()
        .filter_map(|(url, title, tag)| {
            Invoke::apply(&BrowserAction::AddBookmark {
                url: url.into(),
                title: title.into(),
                tag: format!("bookmark:{tag}"),
            })
            .ok()
        })
        .collect()
    }

    fn view(cx: &mut ViewCx<Browser>) -> Element {
        // Only a window someone uses reaches the web: a past version and a
        // screenshot show the deterministic page of the test engine.
        let live = cx.mode().is_live();
        let mode = if cx.is_private() {
            BrowsingMode::Private
        } else {
            BrowsingMode::Normal
        };
        let widget = if live {
            if let Err(error) = crate::storage::prepare_window(mode.is_private()) {
                tracing::error!(%error, "Cannot prepare browser storage");
            }
            BrowserWidget::new_with_mode(mode)
        } else {
            BrowserWidget::new_headless()
        };
        // One window, one page. A launch without a page to show starts in
        // the address field; other pages are other windows, in the
        // desktop's tabs.
        let target = launch_target(cx.launch_arguments());
        let focus_address = live && target.is_none();
        let widget = match target {
            Some(uri) => widget.with_initial_uri(uri),
            None => widget,
        };
        cx.surface(BrowserWindow::new(widget, focus_address))
    }
}

impl ResourceSurface<Browser, BrowserEffect> for BrowserWindow {
    fn present(&mut self, state: &Rc<BrowserState>) {
        self.present_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Browser>) {
        self.attach_dispatcher(cx.dispatcher());
        if cx.mode().is_live() {
            // Visits and addresses are the person's own: they follow them to
            // their other devices and never reach a collaborator. A private
            // window keeps none.
            // Downloads stay on this device, in its settings.
            if !cx.is_private() {
                self.attach_downloads(cx.storage().settings());
            }
            if !cx.is_private()
                && let Some(history) = cx.person::<crate::history::BrowsingHistory>()
            {
                self.attach_history(history);
            }
            if !cx.is_private()
                && let Some(book) = cx.person::<crate::autofill::AddressBook>()
            {
                self.attach_address_book(book);
            }
            // The engine publishes frames and navigation from its own
            // threads; each wakes the window so pages paint and finish
            // loading without waiting for input.
            let waker = cx.waker();
            self.engine_handle()
                .set_waker(Box::new(move || waker.wake()));
        }
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    /// Every effect acts on the window's page; one that waits (a page
    /// loading, the engine answering) is answered once it settles.
    fn effect(&mut self, effect: BrowserEffect, cx: &mut EffectCx<Browser, Self>) -> EffectAnswer {
        let invocation = cx.invocation().unwrap_or_default();
        match self.page_mut().admit(invocation, effect.operation())? {
            ui::widget::ResourceOperationStart::Completed(output) => Ok(Effected::Done(output)),
            ui::widget::ResourceOperationStart::Accepted => {
                let reply = cx.reply();
                self.page_mut().answer_later(invocation, reply);
                Ok(Effected::Pending)
            }
        }
    }

    fn bound_declarations(&self) -> Vec<ActionDecl<Browser>> {
        self.page().page_declarations()
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.page().live_rows()
    }

    /// A history page or download a search hit names, past the published
    /// page of rows.
    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.page().search_row(id)
    }

    /// The person's browsing history and this device's downloads.
    fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        self.page().search_entries()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::testing::TestApp;
    use ui::tilo_actions::OperationKind;

    fn find<'a>(
        resources: &'a [tilo_actions::Resource],
        id: &str,
    ) -> Option<&'a tilo_actions::Resource> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource)
            } else {
                find(&resource.children, id)
            }
        })
    }

    #[test]
    fn the_manifest_asks_for_the_engine_runtime_this_build_drives() {
        let expected = format!(
            "\noptional-runtimes = [\"{}\"]\n",
            ui::webview::chromium::runtime::RUNTIME_REF
        );
        assert!(include_str!("../tilo-app.toml").contains(&expected));
    }

    #[test]
    fn the_window_bookmarks_its_page_and_follows_preferences_through_the_door() {
        let mut app = TestApp::<BrowserShell>::capture(None).unwrap();
        app.harness().click_named("browser.bookmark").unwrap();
        app.settle();
        let state = app.state();
        assert_eq!(state.bookmarks.entries().len(), 1, "the star adds the page");
        let page = state.bookmarks.entries()[0].url.clone();
        assert!(state.bookmarks.contains("https://example.org"));

        let published = app.harness().resources();
        let root = find(&published, Browser::KIND).unwrap();
        assert!(
            root.actions
                .iter()
                .all(|action| action.id != "bookmark-page"),
            "a bookmarked page is not offered again"
        );
        let row = find(&published, &bookmark_resource_id(&page)).unwrap();
        let ids: Vec<&str> = row
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        assert!(ids.contains(&"open") && ids.contains(&"remove"), "{ids:?}");
        for id in [
            "browser.page",
            "browser.history",
            "browser.downloads",
            "browser.library",
        ] {
            let live = find(&published, id).unwrap();
            assert!(
                live.actions
                    .iter()
                    .all(|action| action.kind != OperationKind::Document),
                "{id} is what this window does, never a document edit"
            );
        }

        app.call_on(
            &bookmark_resource_id(&page),
            "remove",
            serde_json::json!({}),
        )
        .unwrap();
        app.settle();
        assert!(app.state().bookmarks.entries().is_empty());
        assert!(
            find(&app.harness().resources(), Browser::KIND)
                .unwrap()
                .actions
                .iter()
                .any(|action| action.id == "bookmark-page"),
            "the page can be bookmarked again"
        );

        app.call_on(
            "browser.settings",
            "set-homepage",
            serde_json::json!({"url": "https://home.test/"}),
        )
        .unwrap();
        app.call("set-adblock", serde_json::json!({"enabled": false}))
            .unwrap();
        app.settle();
        let preferences = app.state().preferences;
        assert_eq!(preferences.homepage, "https://home.test/");
        assert!(!preferences.adblock_enabled);
        assert!(app.undo(), "a preference is an undo step");
        assert!(app.state().preferences.adblock_enabled);

        app.call_on(
            "browser.settings",
            "set-site-protection",
            serde_json::json!({"origin": "https://Tracker.example/page", "enabled": false}),
        )
        .unwrap();
        app.settle();
        assert_eq!(
            app.state().site_exceptions.into_iter().collect::<Vec<_>>(),
            vec!["https://tracker.example".to_string()],
            "a site exception is kept by its exact origin"
        );
        app.call_on(
            "browser.settings",
            "set-site-protection",
            serde_json::json!({"origin": "https://tracker.example", "enabled": true}),
        )
        .unwrap();
        app.settle();
        assert!(app.state().site_exceptions.is_empty());
    }

    #[test]
    fn a_launch_opens_only_the_web_pages_the_browser_claims() {
        let uri = "HTTPS://example.test/a --still-data";
        assert_eq!(launch_target(&[uri.into()]).as_deref(), Some(uri));
        for value in [
            "javascript:alert(1)",
            "data:text/plain,x",
            "mailto:a@b",
            "file:///tmp/portal-bypass.html",
            "/tmp/portal-bypass.html",
        ] {
            assert_eq!(launch_target(&[value.into()]), None);
        }
    }

    #[test]
    fn every_scenario_stages_and_bookmarks_come_from_the_document() {
        for scenario in SCENARIOS {
            let mut app = TestApp::<BrowserShell>::capture(Some(scenario)).unwrap();
            app.settle();
            if *scenario == "library-bookmarks" {
                assert_eq!(app.state().bookmarks.entries().len(), 2);
            }
        }
    }

    #[test]
    fn preferences_read_their_defaults_until_the_document_sets_them() {
        let state = Browser::read(&Doc::new()).unwrap();
        assert_eq!(state.preferences, Settings::default());
        assert!(state.bookmarks.entries().is_empty());
    }
}

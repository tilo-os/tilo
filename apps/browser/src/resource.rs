//! Browser as a [`tilo_resource::Resource`]: bookmarks and preferences are
//! document actions; window navigation is published from
//! [`Resource::local_operations`], and what only this window knows (the
//! page, history, downloads, extensions) from the surface's live rows.

use std::rc::Rc;

use ui::framework::{LiveRow, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionParam, ActionSpec, ParamType};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry, declaration_spec};
use ui::widget::Element;

use crate::actions::{
    BROWSER_ACTION_TYPE, BrowserAction, bookmark_resource_id, bookmarks_from_doc, register,
};
use crate::browser_widget::{BrowserWidget, window_navigation_operations};
use crate::library::{Bookmark, Bookmarks};
use crate::settings::Settings;

/// Document projection: collaborative bookmarks and the preferences that
/// follow the person to every browser window.
#[derive(Clone, Debug, Default)]
pub struct BrowserState {
    pub bookmarks: Bookmarks,
    pub preferences: Settings,
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

    fn local_operations() -> Vec<ActionSpec> {
        window_navigation_operations()
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
}

/// Framework app: the browser chrome is the surface over [`Browser`].
pub struct BrowserShell;

impl ResourceApp for BrowserShell {
    type Resource = Browser;
    const ID: &'static str = "so.tilo.browser";
    const TITLE: &'static str = "Browser";
    const ICON: Option<&'static str> = Some("browser");
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1200, 720)
    }

    fn local_operations() -> Vec<ActionSpec> {
        crate::browser_widget::window_presentation_operations()
    }

    fn view(cx: &mut ViewCx<Browser>) -> Element {
        // Only a window someone uses reaches the web: a past version and a
        // screenshot show the deterministic page of the test engine.
        let widget = if cx.mode().is_live() {
            BrowserWidget::new()
        } else {
            BrowserWidget::new_headless()
        };
        cx.surface(widget)
    }
}

impl ResourceSurface<Browser> for BrowserWidget {
    fn present(&mut self, state: &Rc<BrowserState>) {
        self.present_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Browser>) {
        self.attach_dispatcher(cx.dispatcher());
    }

    fn bound_declarations(&self) -> Vec<ActionDecl<Browser>> {
        self.page_declarations()
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.live_rows()
    }
}

/// Live launch on the framework host.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<BrowserShell>()
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
    }

    #[test]
    fn preferences_read_their_defaults_until_the_document_sets_them() {
        let state = Browser::read(&Doc::new()).unwrap();
        assert_eq!(state.preferences, Settings::default());
        assert!(state.bookmarks.entries().is_empty());
    }
}

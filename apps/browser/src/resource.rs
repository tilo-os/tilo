//! Browser's document: the bookmarks, the preferences that follow the
//! person to every browser window, and the sites tracker protection is off
//! for. Bookmarking a page and turning protection off for a site are
//! verbs; a bookmark's title and every preference are field edits at their
//! paths. What only this window knows (the page, navigation, downloads,
//! extensions) is published from the surface's live rows and effects; the
//! visit log and address book are the person's own records
//! ([`BrowsingHistory`](crate::history::BrowsingHistory),
//! [`AddressBook`](crate::autofill::AddressBook)).

use std::rc::Rc;

use ui::framework::{
    Command, EffectAnswer, EffectCx, Effected, LiveRow, ResourceApp, ResourceSurface, SurfaceCx,
    ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{
    self, ActionCx, ActionDecl, Collection, Derived, Id, Invoke, Model, Refusal, ScopeDecl, Set,
    resource,
};
use ui::widget::Element;

use crate::BrowserWindow;
use crate::actions::{MAX_SITE_EXCEPTIONS, normalized_bookmark_url};
use crate::browser_widget::BrowserWidget;
use crate::browsing_mode::BrowsingMode;
use crate::effects::BrowserEffect;
use crate::library::{Bookmark, MAX_BOOKMARKS};
use crate::settings::Settings;

ui::embedded_catalog!(pub(crate) static CATALOG, ["en-US", "de-DE"]);

/// The browser's document: collaborative bookmarks, the preferences, and
/// the origins tracker protection is off for.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Browser {
    /// Bookmarks
    pub bookmarks: Collection<Bookmark>,
    /// Preferences
    pub preferences: Settings,
    /// Sites without tracker protection, by exact origin
    /// (`https://example.org`). Only turning protection on or off for a
    /// site changes them.
    #[model(readonly)]
    pub site_exceptions: Set<String>,
}

#[resource(
    kind = "browser.application",
    label = "Browser",
    initialize = Browser::default,
    scopes = Browser::scopes,
    children = Browser::sites
)]
impl Browser {
    /// Bookmark a page.
    #[action]
    pub fn add_bookmark(
        &mut self,
        cx: &mut ActionCx,
        #[param("The page's absolute http(s) address")] url: String,
        #[param("Its title; the address when none")] title: Option<String>,
    ) -> Result<(), Refusal> {
        let url = normalized_bookmark_url(&url)
            .ok_or_else(|| Refusal::new("a bookmark is an absolute http(s) URL"))?;
        if self.bookmarks.values().any(|bookmark| bookmark.url == url) {
            return Ok(());
        }
        if self.bookmarks.len() >= MAX_BOOKMARKS {
            return Err(Refusal::new("too many bookmarks; remove some first"));
        }
        let title = title
            .map(|title| title.trim().to_owned())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| url.clone());
        self.bookmarks.push(cx.id(), Bookmark { url, title });
        Ok(())
    }

    /// Remove a bookmark.
    #[action(item = bookmarks)]
    pub fn remove_bookmark(&mut self, bookmark: &Id) -> Result<(), Refusal> {
        let url = self
            .bookmarks
            .get(bookmark)
            .ok_or_else(|| Refusal::new("no such bookmark"))?
            .url
            .clone();
        // Two people bookmarking one page at once made two items of it.
        self.bookmarks.retain(|_, bookmark| bookmark.url != url);
        Ok(())
    }

    /// Turn tracker protection on or off for a site.
    #[action]
    pub fn set_site_protection(
        &mut self,
        #[param("The site's address or exact origin, such as https://example.org")] origin: String,
        #[param("Whether tracker protection is on for the site")] enabled: bool,
    ) -> Result<(), Refusal> {
        let origin = crate::site_controls::origin(origin.trim())
            .ok_or_else(|| Refusal::new("a site is named by one http(s) origin"))?;
        if enabled {
            self.site_exceptions.remove(&origin);
        } else if !self.site_exceptions.contains(&origin) {
            if self.site_exceptions.len() >= MAX_SITE_EXCEPTIONS {
                return Err(Refusal::new(
                    "too many site exceptions; turn protection back on for some",
                ));
            }
            self.site_exceptions.insert(origin);
        }
        Ok(())
    }
}

/// The resource id of the bookmark `id`: its item of the document.
pub fn bookmark_resource_id(id: &Id) -> String {
    tilo_resource::items::item_resource_id::<Browser>("bookmarks", id)
}

impl Browser {
    /// The library's sections a search can be narrowed to.
    fn scopes(_: &Self) -> Vec<ScopeDecl> {
        vec![
            ScopeDecl::new(crate::actions::HISTORY_SCOPE, "History"),
            ScopeDecl::new(crate::actions::BOOKMARKS_SCOPE, "Bookmarks"),
            ScopeDecl::new(crate::actions::DOWNLOADS_SCOPE, "Downloads"),
        ]
    }

    /// Each site tracker protection is off for, as a place that turns it
    /// back on.
    fn sites(&self) -> Vec<Derived<Self>> {
        self.site_exceptions
            .iter()
            .map(|origin| {
                let params = browser::SetSiteProtection::paths();
                Derived::new(origin.clone())
                    .of_kind("site")
                    .identified(format!("site:{origin}"))
                    .with(
                        Self::SET_SITE_PROTECTION
                            .bind(params.origin, origin.clone())
                            .and(params.enabled, true),
                    )
            })
            .collect()
    }

    /// Turning tracker protection for `origin` to the other state from
    /// `protected`, as the row of a page on that site offers it.
    pub(crate) fn site_switch(origin: &str, protected: bool) -> Option<ActionDecl<Self>> {
        let params = browser::SetSiteProtection::paths();
        Self::SET_SITE_PROTECTION
            .bind(params.origin, origin.to_owned())
            .and(params.enabled, !protected)
            .declaration(
                "set-site-protection",
                if protected {
                    "Turn tracker protection off for this site"
                } else {
                    "Turn tracker protection on for this site"
                },
            )
    }

    /// The origins tracker protection is off for.
    pub fn exceptions(&self) -> std::collections::BTreeSet<String> {
        self.site_exceptions.iter().cloned().collect()
    }
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
    "permission",
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

/// The scheme desktop search hands the browser a web search under:
/// `web+search:<percent-encoded query>`. The browser, not the desktop,
/// knows which search engine the person chose.
pub const WEB_SEARCH_SCHEME: &str = "web+search";

/// The web search a launch names (`web+search:cats%20videos`), decoded;
/// `None` for anything else or an empty query.
fn launch_search(arguments: &[String]) -> Option<String> {
    arguments.iter().find_map(|arg| {
        let (scheme, rest) = arg.split_once(':')?;
        if !scheme.eq_ignore_ascii_case(WEB_SEARCH_SCHEME) || arg.len() > 8 * 1024 {
            return None;
        }
        let (query, _) = url::form_urlencoded::parse(rest.as_bytes()).next()?;
        let query = query.trim();
        (!query.is_empty() && !query.chars().any(char::is_control)).then(|| query.to_owned())
    })
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
            Some("loading" | "error" | "permission") => (900, 620),
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
            ("https://www.rust-lang.org/learn", "Learn Rust"),
            ("https://developer.mozilla.org/", "MDN Web Docs"),
        ]
        .into_iter()
        .map(|(url, title)| {
            Browser::ADD_BOOKMARK.with(browser::AddBookmark {
                url: url.into(),
                title: Some(title.into()),
            })
        })
        .collect()
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
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
            BrowserWidget::new_with_mode(mode).with_service_client(
                if !mode.is_private()
                    && cx
                        .me()
                        .and_then(crate::service_sessions::identity)
                        .is_some()
                {
                    ui::collaboration::browser_service_sessions::Client::capture()
                } else {
                    None
                },
            )
        } else {
            BrowserWidget::new_headless()
        };
        // One window, one page. A launch without a page to show starts in
        // the address field; other pages are other windows, in the
        // desktop's tabs.
        let target = launch_target(cx.launch_arguments());
        let search = launch_search(cx.launch_arguments());
        let focus_address = live && target.is_none() && search.is_none();
        let widget = match (target, search) {
            (Some(uri), _) => widget.with_initial_uri(uri),
            (None, Some(query)) => widget.with_initial_search(query),
            (None, None) => widget,
        };
        // A search hit on a bookmark, or an agent's `open`, shows its page.
        let (live, dispatch) = (cx.live().clone(), cx.dispatcher());
        cx.on_open_item(move |bookmark| {
            let url = live
                .with(|browser| browser.bookmarks.get(bookmark).map(|b| b.url.clone()))
                .ok_or_else(|| Refusal::new("there is no such bookmark"))?;
            dispatch.effect(&BrowserEffect::Open { url });
            Ok(())
        });
        // A history or download hit opens its page or shows the download.
        let index = widget.search_index();
        let personal_open = widget.personal_open_handle();
        let dispatch = cx.dispatcher();
        cx.on_open(move |anchor: &str| -> Result<(), Refusal> {
            if let Some((session_id, destination)) =
                crate::service_destinations::parse_anchor(anchor)
            {
                if destination.is_some() {
                    return Err(Refusal::new("Open a channel by its ordinary URL"));
                }
                dispatch.effect(&BrowserEffect::OpenServiceSession { session_id });
                return Ok(());
            }
            if let Some(url) = personal_open.with(|rows| rows.get(anchor).cloned()) {
                dispatch.effect(&BrowserEffect::Open { url });
                return Ok(());
            }
            let effect = index
                .opened(anchor)
                .ok_or_else(|| Refusal::new(format!("`{anchor}` is no longer in the history")))?;
            dispatch.effect(&effect);
            Ok(())
        });
        cx.surface(BrowserWindow::new(widget, focus_address))
    }
}

impl ResourceSurface<Browser, BrowserEffect> for BrowserWindow {
    fn present(&mut self, state: &Rc<Browser>) {
        self.present_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Browser>) {
        self.attach_dispatcher(cx.dispatcher());
        self.service_cx = Some(cx.clone());
        if cx.mode().is_live() {
            if !cx.is_private()
                && cx
                    .me()
                    .and_then(crate::service_sessions::identity)
                    .is_some()
            {
                self.page_mut().attach_service_catalog(cx.waker());
            }
            // Visits and addresses are the person's own: they follow them to
            // their other devices and never reach a collaborator. A private
            // window keeps none.
            // Downloads stay on this device, in its settings.
            if !cx.is_private() {
                self.attach_downloads(cx.storage().settings());
            }
            if !cx.is_private()
                && let Some(bookmarks) = cx.person::<crate::personal_bookmarks::PersonalBookmarks>()
            {
                self.attach_personal_bookmarks(bookmarks);
            }
            if !cx.is_private()
                && let Some(history) = cx.person::<crate::history::BrowsingHistory>()
            {
                self.attach_history(history);
            }
            if !cx.is_private()
                && let Some(person) = cx.me()
            {
                self.page_mut()
                    .attach_continuity(person, cx.files(), cx.waker());
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
            self.page_mut().attach_service_waker(waker.clone());
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
        if matches!(&effect, BrowserEffect::ListServiceSessions) {
            let waker = self
                .service_cx
                .as_ref()
                .ok_or("Account custody is unavailable")?
                .waker();
            self.page_mut().begin_service_list(cx.reply(), waker)?;
            return Ok(Effected::Pending);
        }
        if let BrowserEffect::OpenServiceSession { session_id } = &effect {
            let source = self
                .service_cx
                .as_ref()
                .ok_or("This account could not be opened")?;
            let person = source
                .me()
                .and_then(crate::service_sessions::identity)
                .ok_or("This account needs to be connected again")?;
            let waker = source.waker();
            self.page_mut()
                .begin_service_session(session_id, person, waker)?;
            return Ok(Effected::value(
                serde_json::json!({"status":"opening", "authenticated":false}),
            ));
        }
        if let BrowserEffect::RetryForgetServiceSession { session_id }
        | BrowserEffect::ForgetServiceSession { session_id } = &effect
        {
            {
                let source = self
                    .service_cx
                    .as_ref()
                    .ok_or("This account could not be forgotten")?;
                let person = source
                    .me()
                    .and_then(crate::service_sessions::identity)
                    .ok_or("This account needs to be connected again")?;
                let waker = source.waker();
                let reply = cx.reply();
                self.page_mut()
                    .begin_forget_service_session(session_id, person, reply, waker)?;
                return Ok(Effected::Pending);
            }
        }
        let invocation = cx.invocation().unwrap_or_default();
        match self.page_mut().admit(invocation, effect.operation())? {
            Effected::Pending => {
                let reply = cx.reply();
                self.page_mut().answer_later(invocation, reply);
                Ok(Effected::Pending)
            }
            Effected::Done(output) => Ok(Effected::Done(output)),
        }
    }

    fn bound_declarations(&self) -> Vec<ActionDecl<Browser>> {
        self.page().page_declarations()
    }

    const ROW_DECLARATIONS: bool = true;

    /// The page row turns tracker protection on or off for its site.
    fn row_declarations(&self, row: &str) -> Vec<ActionDecl<Browser>> {
        self.page().row_declarations(row)
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
    use ui::tilo_resource::Resource as _;

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

    /// The window's host asks the root widget each frame whether the app
    /// wants the OS window fullscreen; a page in fullscreen must reach it
    /// through the framework's root, not only through the bare window.
    #[test]
    fn page_fullscreen_reaches_the_root_the_host_polls() {
        use ui::widget::FullscreenWidgetExt as _;
        let mut app = TestApp::<BrowserShell>::capture(None).unwrap();
        assert_eq!(app.harness().root_widget().fullscreen_intent(), Some(false));
        app.with_widget::<crate::BrowserWindow, _>(|page| page.force_content_fullscreen(true))
            .expect("the window shows a page");
        app.settle();
        assert_eq!(app.harness().root_widget().fullscreen_intent(), Some(true));
    }

    #[test]
    fn the_window_bookmarks_its_page_and_follows_preferences_through_the_door() {
        let mut app = TestApp::<BrowserShell>::capture(None).unwrap();
        app.harness().click_named("browser.bookmark").unwrap();
        app.settle();
        let state = app.state();
        assert_eq!(state.bookmarks.len(), 1, "the star adds the page");
        let (bookmark, page) = state
            .bookmarks
            .iter()
            .map(|(id, bookmark)| (id.clone(), bookmark.url.clone()))
            .next()
            .unwrap();
        assert!(crate::library::Bookmarks::of(&state.bookmarks).contains("https://example.org"));

        let published = app.harness().resources();
        let root = find(&published, Browser::KIND).unwrap();
        assert!(
            root.actions
                .iter()
                .all(|action| action.id != "bookmark-page"),
            "a bookmarked page is not offered again"
        );
        let row = find(&published, &bookmark_resource_id(&bookmark)).unwrap();
        let ids: Vec<&str> = row
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        assert!(
            ids.contains(&"open") && ids.contains(&"remove-bookmark"),
            "{ids:?}"
        );
        for id in [
            "browser.page",
            "browser.history",
            "browser.downloads",
            "browser.library",
        ] {
            let live = find(&published, id).unwrap();
            assert!(
                live.actions.iter().all(|action| {
                    action.kind != OperationKind::Document
                        || (id == "browser.page" && action.id == "set-site-protection")
                }),
                "{id} is what this window does; only the page's site switch is a document edit"
            );
        }

        app.call_on(
            &bookmark_resource_id(&bookmark),
            "remove-bookmark",
            serde_json::json!({}),
        )
        .unwrap();
        app.settle();
        assert!(app.state().bookmarks.is_empty(), "{page} is removed");
        assert!(
            find(&app.harness().resources(), Browser::KIND)
                .unwrap()
                .actions
                .iter()
                .any(|action| action.id == "bookmark-page"),
            "the page can be bookmarked again"
        );

        let preferences = Browser::paths().preferences;
        app.call(
            "field-edit",
            serde_json::json!({ "edit": preferences.homepage.replace("https://home.test/") }),
        )
        .unwrap();
        assert!(
            app.call(
                "field-edit",
                serde_json::json!({ "edit": preferences.homepage.replace("not a url") }),
            )
            .is_err(),
            "a homepage is a web address"
        );
        app.call(
            "field-edit",
            serde_json::json!({ "edit": preferences.adblock_enabled.set(false) }),
        )
        .unwrap();
        app.settle();
        let preferences = app.state().preferences;
        assert_eq!(preferences.homepage, "https://home.test/");
        assert!(!preferences.adblock_enabled);
        assert!(app.undo(), "a preference is an undo step");
        assert!(app.state().preferences.adblock_enabled);

        app.call(
            "set-site-protection",
            serde_json::json!({"origin": "https://Tracker.example/page", "enabled": false}),
        )
        .unwrap();
        app.settle();
        assert_eq!(
            app.state().exceptions().into_iter().collect::<Vec<_>>(),
            vec!["https://tracker.example".to_string()],
            "a site exception is kept by its exact origin"
        );
        let published = app.harness().resources();
        let site = find(
            &published,
            "browser.application/derived/site:https://tracker.example",
        )
        .expect("a site without protection is a place");
        let protect = site
            .actions
            .iter()
            .find(|action| action.id == "set-site-protection")
            .expect("the place turns protection back on");
        assert!(
            protect.params().is_empty(),
            "the site and the switch are bound"
        );
        app.call_on(
            "browser.application/derived/site:https://tracker.example",
            "set-site-protection",
            serde_json::json!({}),
        )
        .unwrap();
        app.settle();
        assert!(app.state().site_exceptions.is_empty());

        let page = find(&app.harness().resources(), "browser.page")
            .unwrap()
            .clone();
        let switch = page
            .actions
            .iter()
            .find(|action| action.id == "set-site-protection")
            .expect("the page row switches protection for its site");
        assert!(switch.params().is_empty());
        app.call_on("browser.page", "set-site-protection", serde_json::json!({}))
            .unwrap();
        app.settle();
        assert_eq!(
            app.state().exceptions().into_iter().collect::<Vec<_>>(),
            ["https://example.org"],
            "the page's own site"
        );
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
    fn a_web_search_launch_names_its_query_and_nothing_else() {
        assert_eq!(
            launch_search(&["web+search:cats%20%26%20dogs".into()]).as_deref(),
            Some("cats & dogs")
        );
        assert_eq!(
            launch_search(&["WEB+SEARCH:wetter+k%C3%B6ln".into()]).as_deref(),
            Some("wetter köln")
        );
        for value in [
            "web+search:",
            "web+search:%20",
            "https://example.test/",
            "cats",
        ] {
            assert_eq!(launch_search(&[value.into()]), None, "{value}");
        }
    }

    #[test]
    fn every_scenario_stages_and_bookmarks_come_from_the_document() {
        for scenario in SCENARIOS {
            let mut app = TestApp::<BrowserShell>::capture(Some(scenario)).unwrap();
            app.settle();
            if *scenario == "library-bookmarks" {
                assert_eq!(app.state().bookmarks.len(), 2);
            }
        }
    }

    #[test]
    fn a_new_browser_starts_with_the_default_preferences() {
        let state = ui::tilo_resource::Replica::<Browser>::new()
            .state()
            .unwrap();
        assert_eq!(state.preferences, Settings::default());
        assert!(state.bookmarks.is_empty());
    }
}

#[cfg(test)]
mod model_tests;

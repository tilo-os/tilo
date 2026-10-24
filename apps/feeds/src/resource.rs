//! The library's verbs, the places derived from it, and the app.
//!
//! Plain fields change through the built-in field edits every path offers
//! (`Library::paths().articles.item(&id).starred.set(true)`): reading,
//! starring, tagging, renaming a feed or a folder, moving a feed and folding
//! a folder. The methods below are the operations that are more than
//! setting a field. Tags are derived from the articles and are published as
//! places that reveal what they label.
//!
//! Refreshing and OPML import/export change nothing by themselves: they are
//! the reader's [`FeedsEffect`]s, whose results reach the library as verbs
//! (what a refresh fetched lands through [`Library::refreshed`]). How articles
//! read (text size, face, paper) is the person's [`ReadSettings`], a personal
//! setting the reader's effects change.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, ResourceApp, ResourceSurface, SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{self, ActionCx, Derived, Effect, Id, Invoke, Refusal, resource};
use ui::widget::Element;

use crate::app::FeedsApp;
use crate::model::{
    Article, Feed, Fetched, Filter, Folder, Library, Outline, ReadFont, ReadSettings, ReadTheme,
    tag_key,
};

ui::embedded_catalog!(static CATALOG, ["en-US", "de-DE"]);

/// How many tags the library publishes as places.
pub const MAX_TAG_PLACES: usize = 32;

#[resource(
    kind = "feeds.library",
    label = "Feeds",
    children = Library::places,
    version = 2,
    migrate = crate::model::upgrade
)]
impl Library {
    /// Add a subscription.
    ///
    /// Subscribes to a feed's address. It is titled with its address until a
    /// refresh brings its real title and articles.
    #[action]
    pub fn add_subscription(
        &mut self,
        cx: &mut ActionCx,
        #[param("Feed URL")] url: String,
        #[param("The folder to file it in; none for the top of the sidebar")] folder: Option<Id>,
    ) -> Result<(), Refusal> {
        let url = url.trim();
        if url.is_empty() {
            return Err(Refusal::new("a subscription needs an address"));
        }
        if self.feed_at(url).is_some() {
            return Err(Refusal::new("already subscribed to that address"));
        }
        if folder.as_ref().is_some_and(|id| !self.folders.contains(id)) {
            return Err(Refusal::new("no such folder"));
        }
        self.feeds.push(cx.id(), Feed::subscribing(url, folder));
        Ok(())
    }

    /// Unsubscribe.
    ///
    /// Removes the subscription and every article it brought in.
    #[action(destructive, item = feeds)]
    pub fn unsubscribe(&mut self, feed: &Id) -> Result<(), Refusal> {
        if !self.feeds.contains(feed) {
            return Err(Refusal::new("no such subscription"));
        }
        self.remove_feed(feed);
        Ok(())
    }

    /// Mark all of a subscription's articles as read.
    #[action(housekeeping, item = feeds)]
    pub fn mark_feed_read(&mut self, feed: &Id) -> Result<(), Refusal> {
        if !self.feeds.contains(feed) {
            return Err(Refusal::new("no such subscription"));
        }
        self.mark_all_read(&Filter::Feed(feed.clone()));
        Ok(())
    }

    /// Mark all articles as read.
    #[action(housekeeping)]
    pub fn mark_everything_read(&mut self) {
        self.mark_all_read(&Filter::All);
    }

    /// New folder.
    #[action]
    pub fn new_folder(
        &mut self,
        cx: &mut ActionCx,
        #[param("Name")] name: String,
    ) -> Result<(), Refusal> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Refusal::new("a folder needs a name"));
        }
        self.folders.push(cx.id(), Folder::named(name));
        Ok(())
    }

    /// Delete folder.
    ///
    /// Its subscriptions stay, at the top of the sidebar.
    #[action(destructive, item = folders)]
    pub fn delete_folder(&mut self, folder: &Id) -> Result<(), Refusal> {
        self.folders
            .remove(folder)
            .ok_or_else(|| Refusal::new("no such folder"))?;
        for (_, feed) in self.feeds.iter_mut() {
            if feed.folder.as_ref() == Some(folder) {
                feed.folder = None;
            }
        }
        Ok(())
    }

    /// Subscribe to what an OPML file lists.
    ///
    /// Every address the library does not have yet, in the folders the file
    /// names (made where missing): one step, which one undo takes back.
    /// People import a file with Import OPML…, which lands here; the verb
    /// itself, with its parsed outlines, is for agents and scripts.
    #[action(audience = "agents")]
    pub fn import_subscriptions(
        &mut self,
        cx: &mut ActionCx,
        #[param("The subscriptions the file lists")] outlines: Vec<Outline>,
    ) {
        self.import(&outlines, || cx.id());
    }

    /// Bring in what a refresh fetched for one subscription.
    ///
    /// The articles it does not hold yet, its title as the feed names
    /// itself, and the certificate a Gemini server presented the first time.
    /// Content from the feed, not anyone's edit: no undo takes it back.
    /// How a refresh lands, never something a person picks from a list.
    #[action(external, audience = "agents")]
    pub fn refreshed(
        &mut self,
        cx: &mut ActionCx,
        #[param("The articles it lists")] articles: Vec<Fetched>,
        #[param("The subscription")] feed: Id,
        #[param("The title the feed gives itself")] title: Option<String>,
        #[param("The certificate a Gemini server presented")] certificate_pin: Option<String>,
    ) -> Result<(), Refusal> {
        let subscription = self
            .feeds
            .get_mut(&feed)
            .ok_or_else(|| Refusal::new("no such subscription"))?;
        if let Some(title) = title.filter(|title| !title.trim().is_empty()) {
            subscription.title = title;
        }
        if subscription.certificate_pin.is_none() {
            subscription.certificate_pin = certificate_pin;
        }
        for article in articles {
            if !self.has_article(&feed, &article.url, &article.title) {
                self.articles
                    .push(cx.id(), Article::fetched(feed.clone(), article));
            }
        }
        Ok(())
    }
}

impl Library {
    /// Subscribe to every address `outlines` lists that the library does
    /// not have yet, in the folders they name (made where missing), with
    /// ids from `mint`. Returns how many subscriptions it added.
    pub fn import(&mut self, outlines: &[Outline], mut mint: impl FnMut() -> Id) -> usize {
        let mut added = 0;
        for outline in outlines {
            let url = outline.url.trim();
            if url.is_empty() || self.feed_at(url).is_some() {
                continue;
            }
            let folder = outline
                .folder
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(|name| {
                    let existing = self
                        .folders
                        .iter()
                        .find(|(_, folder)| folder.name == name)
                        .map(|(id, _)| id.clone());
                    existing.unwrap_or_else(|| self.folders.push(mint(), Folder::named(name)))
                });
            let title = outline.title.trim();
            let mut feed = Feed::subscribing(url, folder);
            if !title.is_empty() {
                title.clone_into(&mut feed.title);
            }
            if !outline.site_url.trim().is_empty() {
                outline.site_url.trim().clone_into(&mut feed.site_url);
            }
            self.feeds.push(mint(), feed);
            added += 1;
        }
        added
    }

    /// The places derived from the articles: each tag reveals the articles
    /// it labels.
    fn places(&self) -> Vec<Derived<Self>> {
        self.tags_limited(MAX_TAG_PLACES)
            .into_iter()
            .map(Self::tag_place)
            .collect()
    }

    /// The tag `tag` as a place.
    pub fn tag_place(tag: String) -> Derived<Self> {
        Derived::new(format!("#{tag}"))
            .of_kind("tag")
            .identified(format!("tag:{tag}"))
            .with_effect(&FeedsEffect::RevealTag { tag })
    }
}

/// What the reader does that changes no document by itself; what it brings
/// in reaches the library as its verbs.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "feeds.effects")]
pub enum FeedsEffect {
    /// Refresh all subscriptions.
    #[effect(job)]
    RefreshAll,
    /// Import OPML…
    #[effect(attended)]
    ImportOpml,
    /// Export OPML…
    #[effect(attended, kind = "read")]
    ExportOpml,
    /// Reveal tagged articles
    #[effect(view, row, idempotent)]
    RevealTag { tag: String },
    /// Increase reader text size
    ///
    /// A reading preference of this person, which follows them to their
    /// other devices.
    #[effect(kind = "read")]
    ReaderTextLarger,
    /// Decrease reader text size
    ///
    /// A reading preference of this person, which follows them to their
    /// other devices.
    #[effect(kind = "read")]
    ReaderTextSmaller,
    /// Use reader font
    ///
    /// A reading preference of this person, which follows them to their
    /// other devices.
    #[effect(kind = "read", idempotent)]
    ReaderFont {
        #[param("Reader font", choices = ["sans", "serif", "mono"])]
        font: String,
    },
    /// Use reader paper
    ///
    /// A reading preference of this person, which follows them to their
    /// other devices.
    #[effect(kind = "read", idempotent)]
    ReaderPaper {
        #[param("Reader paper", choices = ["light", "sepia", "dark"])]
        paper: String,
    },
}

/// Framework app.
pub struct FeedsShell;

/// Scenarios that show the reader in a narrow, phone-sized window.
const NARROW_SCENARIOS: [&str; 2] = ["sidebar-open", "detail"];

impl ResourceApp for FeedsShell {
    type Resource = Library;
    const ID: &'static str = "so.tilo.feeds";
    const TITLE: &'static str = "Feeds";
    const ICON: Option<&'static str> = Some("feeds");

    fn view(cx: &mut ViewCx<Library>) -> Element {
        let app = FeedsApp::new();
        // A picked search hit (or an agent's `open`) shows its item.
        cx.on_open_item(app.opener());
        cx.surface(app)
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
    fn scenario(name: Option<&str>) -> Vec<Invoke<Library>> {
        if name == Some("empty") {
            return Vec::new();
        }
        let library = crate::sample::store();
        let paths = Library::paths();
        let edits = whole(&library);
        let mut invokes: Vec<Invoke<Library>> = edits
            .iter()
            .filter_map(|edit| Invoke::edit(edit).ok())
            .collect();
        match name.unwrap_or_default() {
            // Titles long enough that the sidebar elides them to one line
            // with a fade instead of wrapping into the next row.
            "long-titles" => invokes.extend(
                library
                    .feeds
                    .ids()
                    .zip([
                        "International Space Station Daily Operations & Mission Updates",
                        "The Exhaustively Comprehensive Front-End Engineering Weekly Digest",
                    ])
                    .filter_map(|(id, title)| {
                        Invoke::edit(&paths.feeds.item(id).title.replace(title)).ok()
                    }),
            ),
            // The sidebar right after an OPML import folded new folders and
            // feeds in.
            "post-import" => invokes.push(Library::IMPORT_SUBSCRIPTIONS.with(
                crate::opml::outlines(&crate::opml::parse(crate::opml::sample_opml())),
            )),
            _ => {}
        }
        invokes
    }
}

impl ResourceSurface<Library, FeedsEffect> for FeedsApp {
    fn present(&mut self, state: &Rc<Library>) {
        self.present_library(Rc::clone(state));
    }

    fn attach(&mut self, cx: SurfaceCx<Library>) {
        self.attach_dispatcher(cx.dispatcher());
        self.follow_read_settings(cx.settings::<ReadSettings>());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(&mut self, effect: FeedsEffect, cx: &mut EffectCx<Library, Self>) -> EffectAnswer {
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
            FeedsEffect::RevealTag { tag } => {
                self.reveal_tag(&tag)?;
                Ok(Effected::done())
            }
            FeedsEffect::ReaderTextLarger => {
                self.change_read_settings(|s| s.adjust_text_percent(1))?;
                Ok(Effected::done())
            }
            FeedsEffect::ReaderTextSmaller => {
                self.change_read_settings(|s| s.adjust_text_percent(-1))?;
                Ok(Effected::done())
            }
            FeedsEffect::ReaderFont { font } => {
                let font = ReadFont::ALL
                    .into_iter()
                    .find(|candidate| candidate.as_str() == font)
                    .ok_or_else(|| format!("no reader font {font:?}: sans, serif or mono"))?;
                self.change_read_settings(|s| s.font = font)?;
                Ok(Effected::done())
            }
            FeedsEffect::ReaderPaper { paper } => {
                let theme = ReadTheme::ALL
                    .into_iter()
                    .find(|candidate| candidate.as_str() == paper)
                    .ok_or_else(|| format!("no reader paper {paper:?}: light, sepia or dark"))?;
                self.change_read_settings(|s| s.theme = theme)?;
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

/// The field edits that put every folder, subscription and article of
/// `library` into an empty one, under their ids.
pub(crate) fn whole(library: &Library) -> Vec<tilo_resource::Edit<Library>> {
    let paths = Library::paths();
    let folders = library
        .folders
        .iter()
        .enumerate()
        .map(|(at, (id, folder))| paths.folders.insert_as(id, at, folder));
    let feeds = library
        .feeds
        .iter()
        .enumerate()
        .map(|(at, (id, feed))| paths.feeds.insert_as(id, at, feed));
    let articles = library
        .articles
        .iter()
        .enumerate()
        .map(|(at, (id, article))| paths.articles.insert_as(id, at, article));
    folders.chain(feeds).chain(articles).collect()
}

/// The filter that shows the articles tagged `tag`.
pub(crate) fn tag_filter(tag: &str) -> Filter {
    Filter::Tag(tag_key(tag))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::FeedsShell::scenarios() {
            let mut app = TestApp::<super::FeedsShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}

//! The library's verbs, the places derived from it, and the app.
//!
//! What navigates the library — its folders and subscriptions — is this
//! record: plain fields change through the built-in field edits every path
//! offers (renaming a feed or a folder, moving a feed, folding a folder), and
//! the methods below are the operations that are more than setting a field.
//! What a subscription brought in grows without bound and is kept in parts
//! beside it ([`crate::part::ArticlePart`]): an article is read, starred and
//! tagged through the chain's own actions, and the tags a part's articles
//! carry are published there as places that reveal what they label.
//!
//! Refreshing and OPML import/export change nothing by themselves: they are
//! the reader's [`FeedsEffect`]s, whose results reach the resources as verbs
//! (what a refresh fetched lands through [`crate::part::ArticlePart::receive`]
//! and the subscription it fed is noted by [`Library::note_feed`]). How
//! articles read (text size, face, paper) is the person's
//! [`ReadSettings`](crate::model::ReadSettings), a personal
//! setting the reader's effects change.

use ui::framework::{ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
#[cfg(feature = "samples")]
use ui::tilo_resource::Invoke;
use ui::tilo_resource::{self, ActionCx, Effect, Id, NonBlank, Refusal, resource};
use ui::widget::Element;

use crate::model::{Feed, Filter, Folder, Library, Outline};
use crate::part::ArticlePart;

/// How many tags a part of the articles publishes as places.
pub const MAX_TAG_PLACES: usize = 32;

impl Library {
    /// What the resource holds: the folders and the subscriptions.
    ///
    /// It answers through [`fields`](tilo_resource::read::fields): a caller
    /// with no window open reads the resource as it reads any app's resource, and
    /// a named item is read from the resource's table or by the read its own
    /// type gives it.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "feeds.library",
    search = crate::search::entries,
    version = 3,
    migrate = crate::model::upgrade,
    read_content = Library::read_document
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
        #[param("Feed URL")] url: NonBlank,
        #[param("The folder to file it in; none for the top of the sidebar")] folder: Option<Id>,
    ) -> Result<(), Refusal> {
        let url = url.as_str();
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
    /// Removes the subscription. What it brought in is not shown and does not
    /// count from then on; the window takes the room back through the parts
    /// it holds ([`ArticlePart::forget_feed`]), and an undo that brings the
    /// subscription back shows them again.
    #[action(destructive, item = feeds)]
    pub fn unsubscribe(&mut self, feed: &Id) -> Result<(), Refusal> {
        self.feeds
            .remove(feed)
            .ok_or_else(|| Refusal::new("no such subscription"))?;
        Ok(())
    }

    /// New folder.
    #[action]
    pub fn new_folder(&mut self, cx: &mut ActionCx, #[param("Name")] name: NonBlank) {
        self.folders.push(cx.id(), Folder::named(name));
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

    /// Note what a refresh found out about one subscription: the title the
    /// feed gives itself, and the certificate a Gemini server presented the
    /// first time. Content from the feed, not anyone's edit: no undo takes it
    /// back. The articles it brought land in the parts beside the library
    /// ([`ArticlePart::receive`]).
    #[action(external, audience = "agents", item = feeds)]
    pub fn note_feed(
        &mut self,
        #[param("The subscription")] feed: &Id,
        #[param("The title the feed gives itself")] title: Option<String>,
        #[param("The certificate a Gemini server presented")] certificate_pin: Option<String>,
    ) -> Result<(), Refusal> {
        let subscription = self
            .feeds
            .get_mut(feed)
            .ok_or_else(|| Refusal::new("no such subscription"))?;
        if let Some(title) = title.filter(|title| !title.trim().is_empty()) {
            subscription.title = title;
        }
        if subscription.certificate_pin.is_none() {
            subscription.certificate_pin = certificate_pin;
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
    /// New folder…
    ///
    /// Opens the form that names a folder for subscriptions.
    #[effect(view)]
    NewFolderForm,
    /// Export OPML…
    #[effect(attended, job, kind = "read")]
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
    const ID: &'static str = ui::apps::FEEDS;
    const TITLE: &'static str = "Feeds";
    const TITLE_KEY: &'static str = "feeds-app-title";
    const ICON: Option<&'static str> = Some("feeds");

    fn view(cx: &mut ViewCx<Library>) -> Element {
        crate::app::window(cx)
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }

    /// The articles, kept in parts beside the library.
    fn resources(resources: &mut ui::framework::Resources<'_>) {
        resources.chain::<ArticlePart>();
    }

    #[cfg(feature = "samples")]
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
    /// scenario but the first-run `empty` one. Its articles are staged into
    /// the parts as the window opens ([`Self::fixture`]).
    #[cfg(feature = "samples")]
    fn scenario(name: Option<&str>) -> Vec<Invoke<Library>> {
        if name == Some("empty") {
            return Vec::new();
        }
        let (library, _) = crate::sample::store();
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

    /// The articles the bundled sample library brought in, staged into the
    /// parts as the window opens.
    #[cfg(feature = "samples")]
    fn fixture(fixture: &mut ui::framework::Fixture<Library>) {
        if fixture.scenario() != Some("empty") {
            fixture.provide(crate::sample::Scene);
        }
    }
}

/// The field edits that put every folder and subscription of `library` into
/// an empty one, under their ids.
#[cfg(feature = "samples")]
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
    folders.chain(feeds).collect()
}

/// The filter that shows the articles tagged `tag`.
pub(crate) fn tag_filter(tag: &str) -> Filter {
    Filter::Tag(crate::model::tag_key(tag))
}

#[cfg(test)]
mod tests;

ui::reach_tests!(FeedsShell);

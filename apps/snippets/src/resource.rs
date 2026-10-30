//! The library's verbs, the places derived from it, and the app.
//!
//! Plain fields change through the built-in field edits every path offers
//! (`Snippets::paths().snippets.item(&id).favorite.set(true)`): renaming,
//! starring, choosing the language, moving to a folder, tagging and typing
//! in the body. The methods below are the operations that are more than
//! setting a field. Folders and tags are derived from the snippets and are
//! published as places that offer the verbs that act on them.
//!
//! What the window shows (a tag's snippets, a folder's, the search) are
//! [`SnippetsView`]s, which change no document.

use ui::framework::{ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{
    self, ActionCx, Atom, Derived, Effect, Id, Invoke, NonBlank, Refusal, resource,
};
use ui::widget::Element;

use crate::model::{Snippet, Snippets};

ui::embedded_catalog!(static CATALOG, ["en-US", "de-DE"]);

#[resource(kind = "snippets.library", label = "Snippets", children = Snippets::places)]
impl Snippets {
    /// Create a snippet.
    #[action]
    pub fn new_snippet(
        &mut self,
        cx: &mut ActionCx,
        #[param("Title")] title: String,
        #[param("Folder to file it under; none for All snippets")] folder: Option<String>,
    ) {
        let folder = folder.unwrap_or_default();
        self.snippets
            .push(cx.id(), Snippet::titled(title.trim(), folder.trim()));
    }

    /// Duplicate a snippet.
    #[action]
    pub fn duplicate(
        &mut self,
        cx: &mut ActionCx,
        snippet: &Id,
        #[param("Title of the copy; the original's when none")] title: Option<String>,
    ) -> Result<(), Refusal> {
        let original = self.snippets.get(snippet).ok_or("no such snippet")?;
        let copy = Snippet {
            title: title.unwrap_or_else(|| original.title.clone()),
            favorite: false,
            uses: tilo_resource::Counter::default(),
            ..original.clone()
        };
        let after = self
            .snippets
            .position(snippet)
            .map_or(self.snippets.len(), |at| at + 1);
        self.snippets.insert(after, cx.id(), copy);
        Ok(())
    }

    /// Delete a snippet.
    #[action(destructive)]
    pub fn delete(&mut self, snippet: &Id) -> Result<(), Refusal> {
        self.snippets
            .remove(snippet)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such snippet"))
    }

    /// Record a use of a snippet.
    #[action]
    pub fn record_use(&mut self, snippet: &Id) -> Result<(), Refusal> {
        let snippet = self.snippets.get_mut(snippet).ok_or("no such snippet")?;
        snippet.uses.add(1);
        Ok(())
    }

    /// Rename a folder.
    #[action]
    pub fn rename_folder(
        &mut self,
        #[param("Folder")] folder: String,
        #[param("New name")] to: NonBlank,
    ) -> Result<(), Refusal> {
        self.each_matching(
            |s| s.folder.0 == folder,
            |s| s.folder = Atom(to.to_string()),
            "no such folder",
        )
    }

    /// Rename a tag.
    #[action]
    pub fn rename_tag(
        &mut self,
        #[param("Tag")] tag: String,
        #[param("New name")] to: String,
    ) -> Result<(), Refusal> {
        let to = to.trim();
        self.each_matching(
            |s| s.tags.contains(&tag),
            |s| {
                s.tags.remove(&tag);
                s.tags.insert(to.to_owned());
            },
            "no such tag",
        )
    }

    /// Remove a tag from every snippet.
    #[action]
    pub fn remove_tag(&mut self, #[param("Tag")] tag: String) -> Result<(), Refusal> {
        self.each_matching(
            |s| s.tags.contains(&tag),
            |s| {
                s.tags.remove(&tag);
            },
            "no such tag",
        )
    }
}

impl Snippets {
    /// Change every snippet `matches` selects; refused with `missing` when
    /// none does.
    fn each_matching(
        &mut self,
        matches: impl Fn(&Snippet) -> bool,
        mut change: impl FnMut(&mut Snippet),
        missing: &str,
    ) -> Result<(), Refusal> {
        let mut found = false;
        for (_, snippet) in self.snippets.iter_mut() {
            if matches(snippet) {
                change(snippet);
                found = true;
            }
        }
        if found {
            Ok(())
        } else {
            Err(Refusal::new(missing))
        }
    }

    /// The snippets `matches` selects, as a read of a folder or tag answers
    /// them: each by its item, title, language and star.
    pub fn read_place(
        &self,
        kind: &str,
        name: &str,
        matches: impl Fn(&Snippet) -> bool,
    ) -> serde_json::Value {
        serde_json::json!({
            "kind": kind,
            "name": name,
            "snippets": self
                .snippets
                .iter()
                .filter(|(_, snippet)| matches(snippet))
                .map(|(id, snippet)| serde_json::json!({
                    "item": tilo_resource::items::item_resource_id::<Self>("snippets", id),
                    "title": snippet.title,
                    "language": snippet.language.name(),
                    "favorite": snippet.favorite,
                }))
                .collect::<Vec<_>>(),
        })
    }

    /// The places derived from the snippets: each folder reads and reveals
    /// its snippets, files a new one under itself and renames; each tag
    /// reads and reveals its snippets, renames or comes off. A place's read
    /// is what discloses it, so its verbs are offered to whoever may read
    /// it, an agent too, where revealing it is this window's own view.
    fn places(&self) -> Vec<Derived<Self>> {
        let folders = self.folders().into_iter().map(Self::folder_place);
        let tags = self.tags().into_iter().map(|(tag, _)| Self::tag_place(tag));
        folders.chain(tags).collect()
    }

    /// The folder `folder` as a place.
    pub fn folder_place(folder: String) -> Derived<Self> {
        Derived::new(folder.clone())
            .of_kind("folder")
            .identified(format!("folder:{folder}"))
            .with_effect(&SnippetsView::ReadFolder {
                folder: folder.clone(),
            })
            .with_effect(&SnippetsView::RevealFolder {
                folder: folder.clone(),
            })
            .with(
                Self::NEW_SNIPPET.bind(snippets::NewSnippet::paths().folder, Some(folder.clone())),
            )
            .with(Self::RENAME_FOLDER.bind(snippets::RenameFolder::paths().folder, folder))
    }

    /// The tag `tag` as a place.
    pub fn tag_place(tag: String) -> Derived<Self> {
        Derived::new(tag.clone())
            .of_kind("tag")
            .identified(format!("tag:{tag}"))
            .with_effect(&SnippetsView::ReadTag { tag: tag.clone() })
            .with_effect(&SnippetsView::Reveal { tag: tag.clone() })
            .with(Self::RENAME_TAG.bind(snippets::RenameTag::paths().tag, tag.clone()))
            .with(Self::REMOVE_TAG.bind(snippets::RemoveTag::paths().tag, tag))
    }
}

/// What the library shows and answers, which changes no document. Reading
/// and revealing are offered on the tag or folder they name, never with a
/// name to type.
#[derive(Debug, Clone, PartialEq, Eq, Effect)]
#[effect(operation = "snippets.view")]
pub enum SnippetsView {
    /// Reveal tagged snippets
    #[effect(view, row)]
    Reveal { tag: String },
    /// Reveal snippets in a folder
    #[effect(view, row)]
    RevealFolder { folder: String },
    /// Search snippets
    #[effect(view)]
    Search,
    /// Read a tag
    ///
    /// The snippets that carry the tag, each by its item, title, language
    /// and star.
    #[effect(view, row, returns, tag = ui::tilo_actions::DOCUMENT_READ_TAG)]
    ReadTag { tag: String },
    /// Read a folder
    ///
    /// The snippets filed under the folder, each by its item, title,
    /// language and star.
    #[effect(view, row, returns, tag = ui::tilo_actions::DOCUMENT_READ_TAG)]
    ReadFolder { folder: String },
}

/// Framework app: the snippets widget over [`Snippets`].
pub struct SnippetsShell;

impl ResourceApp for SnippetsShell {
    type Resource = Snippets;
    const ID: &'static str = "so.tilo.snippets";
    const TITLE: &'static str = "Snippets";
    const ICON: Option<&'static str> = Some("snippets");

    fn view(cx: &mut ViewCx<Snippets>) -> Element {
        crate::app::window(cx)
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    /// `narrow` shows the sidebar as an open overlay flap over the content;
    /// `detail` the single-pane push stack on the opened snippet with its
    /// Back affordance; `empty` the zero state of a fresh install.
    fn scenarios() -> &'static [&'static str] {
        &["narrow", "detail", "empty"]
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            Some("narrow") => (720, 720),
            Some("detail") => (420, 760),
            _ => (1100, 720),
        }
    }

    /// The starter library, so a capture shows the manager doing real work;
    /// `empty` shows a fresh install, which has none.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Snippets>> {
        if name == Some("empty") {
            return Vec::new();
        }
        crate::seed::starter_library()
            .iter()
            .enumerate()
            .filter_map(|(at, snippet)| {
                Invoke::edit(&Snippets::paths().snippets.insert(at, snippet)).ok()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;

ui::reach_tests!(SnippetsShell);

#[cfg(test)]
mod agent_disclosure_tests {
    use ui::framework::ResourceApp;
    use ui::framework::testing::TestApp;
    use ui::tilo_actions::ResourceSpec;

    /// The document read of the first resource of `kind` any scenario
    /// publishes, invoked as `tilo_read` on it does.
    fn read_first(kind: &str) -> ui::serde_json::Value {
        for scenario in <super::SnippetsShell as ResourceApp>::scenarios() {
            let mut app = TestApp::<super::SnippetsShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            app.settle();
            let resources = app.harness().resources();
            if let Some((resource, read)) = document_read(&resources, kind) {
                return app
                    .call_on(&resource, &read, ui::serde_json::json!({}))
                    .unwrap_or_else(|error| panic!("{kind} in {scenario}: {error}"))
                    .unwrap_or_else(|| panic!("the read of {kind} answers"));
            }
        }
        panic!("no scenario publishes a {kind} with a document read");
    }

    fn document_read(resources: &[ResourceSpec], kind: &str) -> Option<(String, String)> {
        resources.iter().find_map(|resource| {
            resource
                .actions
                .iter()
                .find(|action| resource.kind == kind && action.contract.reads_document())
                .map(|read| (resource.id.as_str().to_owned(), read.id.clone()))
                .or_else(|| document_read(&resource.children, kind))
        })
    }

    #[test]
    fn a_tag_and_a_folder_are_read_through_their_document_reads() {
        let answer = read_first("snippets.library.tag");
        assert!(answer["snippets"][0]["title"].is_string(), "{answer}");
        let answer = read_first("snippets.library.folder");
        assert!(answer["snippets"][0]["item"].is_string(), "{answer}");
    }

    #[test]
    fn every_scenario_shows_an_agent_its_actions() {
        if let Err(failures) =
            ui::framework::testing::check_agents_are_shown_every_action::<super::SnippetsShell>()
        {
            panic!("{failures}");
        }
    }
}

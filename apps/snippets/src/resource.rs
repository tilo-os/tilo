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

use std::rc::Rc;

use ui::framework::{EffectAnswer, EffectCx, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{self, ActionCx, Atom, Derived, Effect, Id, Invoke, Refusal, resource};
use ui::widget::Element;

use crate::app::SnippetsApp;
use crate::model::{Snippet, Snippets};

/// Snippets' Fluent catalogs, source locale first.
pub const LOCALES: &[(&str, &str)] = &[
    ("en-US", include_str!("../locales/en-US.ftl")),
    ("de-DE", include_str!("../locales/de-DE.ftl")),
];

static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new("en-US", LOCALES).with_system_locale();

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
        #[param("New name")] to: String,
    ) -> Result<(), Refusal> {
        let to = to.trim();
        if to.is_empty() {
            return Err(Refusal::new("a folder needs a name"));
        }
        self.each_matching(
            |s| s.folder.0 == folder,
            |s| s.folder = Atom(to.to_owned()),
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

    /// The places derived from the snippets: each folder reveals its
    /// snippets, files a new one under itself and renames; each tag reveals
    /// its snippets, renames or comes off.
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
            .with_effect(&SnippetsView::Reveal { tag: tag.clone() })
            .with(Self::RENAME_TAG.bind(snippets::RenameTag::paths().tag, tag.clone()))
            .with(Self::REMOVE_TAG.bind(snippets::RemoveTag::paths().tag, tag))
    }
}

/// What the library shows, which changes no document. Revealing is
/// offered on the tag or folder it shows, never with a name to type.
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
}

/// Framework app: the snippets widget over [`Snippets`].
pub struct SnippetsShell;

impl ResourceApp for SnippetsShell {
    type Resource = Snippets;
    const ID: &'static str = "so.tilo.snippets";
    const TITLE: &'static str = "Snippets";
    const ICON: Option<&'static str> = Some("snippets");

    fn view(cx: &mut ViewCx<Snippets>) -> Element {
        let app = SnippetsApp::new();
        // A search hit, or an agent's `open`, shows the snippet it names.
        cx.on_open_item(app.opener());
        cx.surface(app)
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

impl ResourceSurface<Snippets, SnippetsView> for SnippetsApp {
    fn present(&mut self, state: &Rc<Snippets>) {
        self.show_library(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Snippets>) {
        self.attach_dispatcher(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(&mut self, view: SnippetsView, _: &mut EffectCx<Snippets, Self>) -> EffectAnswer {
        self.show(view)
    }
}

#[cfg(test)]
mod tests;

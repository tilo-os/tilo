//! Snippets as a [`tilo_resource::Resource`]: one action vocabulary, one
//! published tree for the library.
//!
//! Document mutations (`new-snippet`, per-folder create, per-snippet rename /
//! language / tags / star / delete, the body as editable text) live on this
//! resource via [`Resource::declarations`] / [`Resource::children`]. Opening a
//! snippet, revealing a tag and focusing search are [`SnippetsView`]s the
//! surface answers; the first two ride the snippet or tag they show.

use std::rc::Rc;

use ui::framework::{EffectAnswer, EffectCx, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_actions::{ActionParam, ParamType, Resource as PublishedResource};
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;
use uuid::Uuid;

use crate::actions::{
    SNIPPETS_ACTION_TYPE, SnippetsAction, SnippetsView, create_snippet_action,
    open_snippet_address, publish_children, register, reveal_tag_address, titled_snippet,
};
use crate::app::SnippetsApp;
use crate::doc::SnippetsDoc;
use crate::model::Snippet;

// Snippets' Fluent catalogs, source locale first.
ui::embedded_catalog!(pub(crate) static CATALOG, "snippets-library", ["en-US", "de-DE"]);

/// The snippets library resource.
pub struct Snippets;

impl Resource for Snippets {
    const KIND: &'static str = "snippets.library";
    const LABEL: &'static str = "Snippets";
    const ACTION_TYPE: &'static str = SNIPPETS_ACTION_TYPE;

    type State = Vec<Snippet>;
    type Action = SnippetsAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<Vec<Snippet>, ModelError> {
        Ok(SnippetsDoc::list(doc))
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "new-snippet",
                "New snippet",
                vec![
                    ActionParam::required("title", ParamType::Text, "Title"),
                    ActionParam::optional(
                        "folder",
                        ParamType::Text,
                        "Folder to file it under (blank = All Snippets)",
                    ),
                ],
                |args, _| {
                    Ok(create_snippet_action(titled_snippet(
                        args.text("title").unwrap_or_default(),
                        args.text("folder").unwrap_or_default().trim(),
                    )))
                },
            )
            .describe("Create a new snippet from a typed title."),
        ]
    }

    fn children(state: &Vec<Snippet>) -> Vec<PublishedResource> {
        publish_children(state)
    }

    /// Every snippet is found by its title, language, folder, tags,
    /// description or body, and opens; every tag reveals its snippets.
    fn search(state: &Vec<Snippet>) -> Vec<SearchEntry> {
        let mut tags = std::collections::BTreeMap::<&str, usize>::new();
        for tag in state.iter().flat_map(|snippet| &snippet.tags) {
            *tags.entry(tag).or_default() += 1;
        }
        let snippets = state.iter().map(|snippet| {
            let title = if snippet.title.trim().is_empty() {
                "Untitled snippet".to_owned()
            } else {
                snippet.title.clone()
            };
            let text = [
                snippet.language.label(),
                &snippet.folder,
                &snippet.tags.join(" "),
                &snippet.description,
                &snippet.body,
            ]
            .join("\n");
            SearchEntry::new(title, text).invoking(open_snippet_address(snippet.id))
        });
        let tags = tags.into_iter().map(|(tag, count)| {
            SearchEntry::new(
                format!("#{tag}"),
                format!("{count} snippet{}", if count == 1 { "" } else { "s" }),
            )
            .invoking(reveal_tag_address(tag))
        });
        snippets.chain(tags).collect()
    }
}

/// Framework app: the snippets widget over [`Snippets`].
pub struct SnippetsShell;

impl ResourceApp for SnippetsShell {
    type Resource = Snippets;
    const ID: &'static str = "so.tilo.snippets";
    const TITLE: &'static str = "Snippets";
    const ICON: Option<&'static str> = Some("snippets");

    fn view(cx: &mut ViewCx<Snippets>) -> Element {
        cx.surface(SnippetsApp::new())
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
            .into_iter()
            .zip(1_u128..)
            .filter_map(|(mut snippet, n)| {
                snippet.id = Uuid::from_u128(n);
                Invoke::apply(&create_snippet_action(snippet)).ok()
            })
            .collect()
    }
}

impl ResourceSurface<Snippets, SnippetsView> for SnippetsApp {
    fn present(&mut self, state: &Rc<Vec<Snippet>>) {
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
mod tests {
    use super::*;
    use crate::actions::{
        body_resource_id, folder_resource_id, snippet_resource_id, tag_resource_id,
    };
    use ui::prelude::TestApp;

    fn has_control(app: &mut TestApp<SnippetsShell>, label: &str) -> bool {
        app.harness()
            .a11y_nodes()
            .iter()
            .any(|(_, node)| node.label() == Some(label))
    }

    /// A new window starts on an empty library; captures stage the starter
    /// library through actions and their view state through `stage`.
    #[test]
    fn captures_stage_the_starter_library_and_a_new_window_is_empty() {
        let live = TestApp::<SnippetsShell>::new().unwrap();
        assert!(live.state().is_empty(), "a fresh install has no snippets");

        let mut empty = TestApp::<SnippetsShell>::capture(Some("empty")).unwrap();
        assert!(empty.state().is_empty());
        assert!(has_control(&mut empty, "New snippet"));

        let mut detail = TestApp::<SnippetsShell>::capture(Some("detail")).unwrap();
        assert_eq!(detail.harness().inspection().viewport, [420, 760]);
        assert!(detail.state().len() >= 10);
        assert!(has_control(&mut detail, "Back"), "the detail is pushed in");
        detail.harness().click_named("nav-shell-back").unwrap();
        detail.settle();
        assert!(
            !has_control(&mut detail, "Back"),
            "Back returns to the snippet list"
        );

        let mut narrow = TestApp::<SnippetsShell>::capture(Some("narrow")).unwrap();
        assert_eq!(narrow.harness().inspection().viewport, [720, 720]);
        assert!(
            has_control(&mut narrow, "Hide sidebar"),
            "the overlay sidebar is open"
        );
        narrow.harness().click_named("nav-shell-sidebar").unwrap();
        narrow.settle();
        assert!(
            has_control(&mut narrow, "Show sidebar"),
            "the sidebar closes through its control"
        );
    }

    /// Agents and the palette reach every snippet, tag and folder through the
    /// one published tree, and each invocation is the library's action.
    #[test]
    fn the_published_tree_edits_and_opens_snippets() {
        let mut app = TestApp::<SnippetsShell>::new().unwrap();
        app.call(
            "new-snippet",
            serde_json::json!({"title": "Deploy", "folder": "Ops"}),
        )
        .unwrap();
        let deploy = app.state()[0].clone();
        assert_eq!(
            (deploy.title.as_str(), deploy.folder.as_str()),
            ("Deploy", "Ops")
        );

        let surface = app.agent_surface().to_string();
        assert!(surface.contains("Edit body"), "the catalog labels the tree");
        app.call_on(
            &folder_resource_id("Ops"),
            "new-snippet",
            serde_json::json!({"title": "Rollback"}),
        )
        .unwrap();
        let rollback = app
            .state()
            .into_iter()
            .find(|s| s.title == "Rollback")
            .expect("a folder files a new snippet under itself");
        assert_eq!(rollback.folder, "Ops");

        let snippet = snippet_resource_id(deploy.id);
        app.call_on(
            &snippet,
            "set-tags",
            serde_json::json!({"tags": "k8s, release"}),
        )
        .unwrap();
        app.call_on(
            &snippet,
            "set-language",
            serde_json::json!({"language": "shell"}),
        )
        .unwrap();
        app.call_on(
            &body_resource_id(deploy.id),
            "replace-text",
            serde_json::json!({"text": "kubectl rollout restart deploy/${name}"}),
        )
        .unwrap();
        let deploy = app.state().into_iter().find(|s| s.id == deploy.id).unwrap();
        assert_eq!(deploy.tags, ["k8s", "release"]);
        assert_eq!(deploy.language, crate::model::Language::Shell);
        assert_eq!(deploy.body, "kubectl rollout restart deploy/${name}");
        assert!(app.undo(), "the body edit is one undo step");
        assert_eq!(
            app.state().iter().find(|s| s.id == deploy.id).unwrap().body,
            ""
        );

        app.call_on(&snippet, "open", serde_json::json!({}))
            .unwrap();
        app.call_on(&tag_resource_id("k8s"), "reveal", serde_json::json!({}))
            .unwrap();
        app.call("search", serde_json::json!({})).unwrap();
        assert!(
            !surface.contains("\"snippet\""),
            "opening is offered on each snippet, never with an id to type"
        );
        assert!(
            app.call_on(
                &snippet_resource_id(Uuid::nil()),
                "open",
                serde_json::json!({})
            )
            .is_err(),
            "a snippet that does not exist is not opened"
        );

        app.call_on(&snippet, "delete", serde_json::json!({}))
            .expect_err("deleting asks for confirmation");
        assert_eq!(app.state().len(), 2);
    }

    /// Search finds a snippet by anything it holds and opens it; a tag
    /// reveals its snippets.
    #[test]
    fn search_indexes_every_field_and_invokes_the_published_actions() {
        let mut snippet = Snippet::blank("Ops");
        snippet.title = "Deploy".into();
        snippet.tags = vec!["k8s".into()];
        snippet.body = "kubectl rollout".into();
        let entries = Snippets::search(&vec![snippet.clone()]);
        let hit = entries.iter().find(|e| e.title == "Deploy").unwrap();
        for field in ["Ops", "k8s", "kubectl rollout", snippet.language.label()] {
            assert!(hit.text.contains(field), "{field} in {:?}", hit.text);
        }
        assert_eq!(hit.action, Some(open_snippet_address(snippet.id)));
        let tag = entries.iter().find(|e| e.title == "#k8s").unwrap();
        assert_eq!(tag.action, Some(reveal_tag_address("k8s")));
    }
}

//! Browser as a [`tilo_resource::Resource`]: bookmarks and preferences are
//! document actions; navigation and the visit log stay View/Effect operations.

use std::rc::Rc;

use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionParam, ParamType};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    BROWSER_ACTION_TYPE, BrowserAction, bookmark_resource_id, bookmarks_from_doc, register,
};
use crate::browser_widget::BrowserWidget;
use crate::library::{Bookmark, Bookmarks};

/// Document projection: collaborative bookmarks.
#[derive(Clone, Debug, Default)]
pub struct BrowserState {
    pub bookmarks: Bookmarks,
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
        vec![bookmarks]
    }

    fn search(state: &BrowserState) -> Vec<SearchEntry> {
        state
            .bookmarks
            .entries()
            .iter()
            .map(|entry| SearchEntry {
                anchor: Some(bookmark_resource_id(&entry.url)),
                title: if entry.title.is_empty() {
                    entry.url.clone()
                } else {
                    entry.title.clone()
                },
                text: entry.url.clone(),
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
    // Remove/open stay on the surface tree (need observed tags / navigation
    // context). The child is still addressable for search and APPLY.
    tilo_actions::Resource::new(bookmark_resource_id(&entry.url), "browser.bookmark", title)
}

/// Framework app: the browser chrome is the surface over [`Browser`].
pub struct BrowserShell;

impl ResourceApp for BrowserShell {
    type Resource = Browser;
    const ID: &'static str = "so.tilo.browser";
    const TITLE: &'static str = "Browser";
    const ICON: Option<&'static str> = Some("browser");
    const SHOT_VIEWPORT: (u32, u32) = (1200, 720);

    fn view(cx: &mut ViewCx<Browser>) -> Element {
        cx.surface(BrowserWidget::new())
    }
}

impl ResourceSurface<Browser> for BrowserWidget {
    fn present(&mut self, _state: &Rc<BrowserState>) {}

    fn attach(&mut self, _cx: SurfaceCx<Browser>) {
        self.set_framework_hosted(true);
    }
}

/// Live launch on the framework host.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<BrowserShell>()
}

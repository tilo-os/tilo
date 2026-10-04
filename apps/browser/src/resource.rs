//! Browser as a [`tilo_resource::Resource`]: bookmarks and preferences are
//! document actions; window navigation is published from
//! [`Resource::local_operations`].

use std::rc::Rc;

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionParam, ActionSpec, EncodedAction, ParamType};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    BROWSER_ACTION_TYPE, BrowserAction, bookmark_resource_id, bookmarks_from_doc, register,
};
use crate::browser_widget::{BrowserWidget, settings_resource, window_navigation_operations};
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

    fn local_operations() -> Vec<ActionSpec> {
        // Window chrome navigation. Live availability (can-go-back, in-flight
        // stop) stays on the standalone page child; publish always offers the
        // ops so hosted ResourceRoot does not drop the palette door.
        window_navigation_operations()
    }

    fn children(state: &BrowserState) -> Vec<tilo_actions::Resource> {
        let mut bookmarks =
            tilo_actions::Resource::new("browser.bookmarks", "browser.bookmarks", "Bookmarks");
        if let Some(spec) = crate::browser_widget::bookmark_library_show() {
            bookmarks = bookmarks.with_action(spec);
        }
        for entry in state.bookmarks.entries() {
            bookmarks = bookmarks.with_child(bookmark_child(entry));
        }
        vec![bookmarks, settings_resource()]
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
                action: None,
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
    // Open stays on the surface: it navigates this window. Remove is the
    // document action, addressed from the tags the document still holds.
    let mut child =
        tilo_actions::Resource::new(bookmark_resource_id(&entry.url), "browser.bookmark", title);
    if let Ok(spec) = ActionSpec::immediate(
        "remove",
        "Remove bookmark",
        BROWSER_ACTION_TYPE,
        &BrowserAction::RemoveBookmark {
            url: entry.url.clone(),
            observed_tags: entry.tags.clone(),
        },
    ) {
        child = child.with_action(spec);
    }
    child
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

    fn view(cx: &mut ViewCx<Browser>) -> Element {
        cx.surface(BrowserWidget::new())
    }
}

impl ResourceSurface<Browser> for BrowserWidget {
    fn present(&mut self, _state: &Rc<BrowserState>) {
        // Empty on purpose: bookmarks sync through the widget's Automerge path.
        // Wiping chrome/engine state here would drop unsynced local navigation.
    }

    fn attach(&mut self, cx: SurfaceCx<Browser>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone,
/// `--shots`, and tests. Bookmarks and preferences are [`BrowserAction`]s.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Browser>>,
    /// Set by [`Self::attach`] and by tests that have no dispatcher.
    hosted: bool,
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Browser>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    /// Hosted when the framework dispatcher is installed, or when a test
    /// raised the flag without one.
    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted || self.framework.is_some()
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn set_hosted(&mut self, hosted: bool) {
        self.hosted = hosted;
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &BrowserAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = EncodedAction::encode(type_id, action).ok();
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, type_id, action, context),
        }
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }
}

/// Live launch on the framework host.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<BrowserShell>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use ui::resources::PublishesResources;
    use ui::tilo_actions::OperationKind;
    use ui::widget::Widget;

    #[test]
    fn resource_publish_owns_navigation_and_hosted_keeps_no_duplicate_root() {
        let state = BrowserState::default();
        let published = ui::tilo_resource::publish::<Browser>("browser", "Browser", &state);
        assert_eq!(published.kind, "browser.application");
        let ids: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        for expected in ["back", "forward", "reload", "stop", "navigate"] {
            assert!(
                ids.contains(&expected),
                "navigation `{expected}` must ride local_operations via publish: {ids:?}"
            );
        }
        let back = published
            .actions
            .iter()
            .find(|action| action.id == "back")
            .expect("back");
        assert_eq!(back.kind, OperationKind::View);
        assert_eq!(back.operation_type.as_deref(), Some("browser.operation"));
        let navigate = published
            .actions
            .iter()
            .find(|action| action.id == "navigate")
            .expect("navigate");
        assert_eq!(navigate.kind, OperationKind::Effect);

        let mut widget = BrowserWidget::new_headless();
        let mut doc = AutoCommit::new();
        widget.sync(&mut doc, &ui::automerge::ROOT);
        assert!(
            !widget.is_framework_hosted(),
            "standalone stays on the local host"
        );
        let standalone = widget.resources(&doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "browser.application");
        let page = standalone[0]
            .children
            .iter()
            .find(|child| child.id.0 == "browser.page")
            .expect("standalone still publishes the page child");
        assert!(
            page.actions.iter().any(|a| a.id == "back")
                && page.actions.iter().any(|a| a.id == "forward")
                && page.actions.iter().any(|a| a.id == "reload")
                && page.actions.iter().any(|a| a.id == "stop"),
            "standalone page child still carries navigation"
        );

        widget.set_framework_hosted(true);
        assert!(widget.is_framework_hosted());
        assert!(ui::resources::resource(&widget, &doc, "browser").is_none());
        assert!(
            ui::resources::resource(&widget, &doc, "browser.bookmarks").is_some_and(|bookmarks| {
                bookmarks.actions.iter().any(|action| action.id == "show")
            }),
            "hosted bookmarks stay the published row"
        );
        let extras = widget.resources(&doc);
        assert!(
            extras.is_empty(),
            "hosted must not republish browser.application beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn hosted_bookmark_edit_is_submitted_as_a_browser_action() {
        let mut widget = BrowserWidget::new_headless();
        let mut doc = AutoCommit::new();
        widget.sync(&mut doc, &ui::automerge::ROOT);
        widget.set_framework_hosted(true);
        assert!(widget.is_framework_hosted());

        let bookmark = BrowserAction::AddBookmark {
            url: "https://example.test/".into(),
            title: "Example".into(),
            tag: "bookmark:hosted".into(),
        };
        widget.submit_library_action(&mut doc, &bookmark);
        let recorded = widget
            .last_library_action()
            .expect("hosted bookmark edit is submitted");
        let expected = ui::tilo_actions::EncodedAction::encode(BROWSER_ACTION_TYPE, &bookmark)
            .expect("bookmark encodes");
        assert_eq!(recorded.type_id, expected.type_id);
        assert_eq!(recorded.payload, expected.payload);
        assert!(
            bookmarks_from_doc(&doc).is_some_and(|book| {
                book.entries()
                    .iter()
                    .any(|entry| entry.url == "https://example.test/")
            }),
            "the submitted BrowserAction is what the document stores"
        );

        let preference = BrowserAction::SetAdblock { enabled: false };
        widget.submit_library_action(&mut doc, &preference);
        let recorded = widget
            .last_library_action()
            .expect("hosted preference edit is submitted");
        let expected = ui::tilo_actions::EncodedAction::encode(BROWSER_ACTION_TYPE, &preference)
            .expect("preference encodes");
        assert_eq!(recorded.type_id, expected.type_id);
        assert_eq!(recorded.payload, expected.payload);
        assert_eq!(
            ui::doc::read_bool(&doc, &automerge::ROOT, crate::settings::ADBLOCK_PATH),
            Some(false),
            "the preference action is applied to the document"
        );

        let extras = widget.resources(&doc);
        assert!(
            extras.iter().all(|resource| {
                resource.kind != Browser::KIND && resource.id.as_str() != "browser"
            }),
            "hosted must not republish the document root: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
    }
}

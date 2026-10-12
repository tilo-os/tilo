//! Opening the platform search from inside an app: a search launcher or
//! filter escalation opens this window's palette narrowed and pre-filled,
//! `/` opens it when nothing types the slash, and a scope declared by a
//! provider is a "Search in" row.

use super::*;
use crate::search::{ScopeDecl, SearchRequest, SearchResult, search_button};
use crate::widget::TaffyTreeExt;
use crate::widget::{Button, ModelPath, Widget};
use automerge::AutoCommit;
use std::collections::HashMap;
use tilo_actions::{ActionSpec, Resource};

/// Mail stand-in: anchors are paths under their folder.
struct Mailbox;

impl crate::search::SearchProvider for Mailbox {
    fn name(&self) -> &'static str {
        "Messages"
    }
    fn search(&self, query: &str) -> Vec<SearchResult> {
        [
            ("folder:inbox/msg:1", "Invoice from Ada"),
            ("folder:sent/msg:2", "Invoice to Grace"),
        ]
        .into_iter()
        .filter(|(_, title)| title.to_lowercase().contains(&query.to_lowercase()))
        .map(|(anchor, title)| SearchResult::new("Messages", title, anchor))
        .collect()
    }
    fn search_scopes(&self) -> Vec<ScopeDecl> {
        vec![
            ScopeDecl::new("folder:inbox", "Inbox"),
            ScopeDecl::new("folder:sent", "Sent"),
        ]
    }
}

/// An app root with a search launcher in its chrome.
struct MailRoot {
    search: Button,
}

impl Widget for MailRoot {
    crate::provides!(
        dyn crate::resources::PublishesResources,
        dyn crate::search::capability::SearchSource
    );

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.search.build_layout(taffy, theme, node_map);
        taffy.widget_node(taffy::Style::default(), &[child], node_map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let child = taffy.children(node).unwrap()[0];
        self.search.apply_layout(taffy, child, origin);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.search]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.search]
    }
    fn name(&self) -> &'static str {
        "mail-root"
    }
}

impl crate::search::capability::SearchSource for MailRoot {
    fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
        vec![Box::new(Mailbox)]
    }
    fn activate_search_result(&mut self, _: &str, _: &mut crate::widget::Edits<'_>, _: &ModelPath) {
    }
}

impl crate::resources::PublishesResources for MailRoot {
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        let archive = |id: &str, label: &str| {
            Resource::new(id, "mail.folder", label).with_action(
                ActionSpec::operation(
                    "archive",
                    format!("Archive {label}"),
                    tilo_actions::OperationKind::View,
                    "test.mail.archive",
                    &(),
                )
                .unwrap(),
            )
        };
        vec![
            archive("folder:inbox", "Inbox"),
            archive("folder:sent", "Sent"),
        ]
    }
}

fn mail_app(request: SearchRequest) -> App {
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(MailRoot {
            search: search_button(request),
        }),
    );
    app.enable_document_history_chrome();
    app.init_layout();
    app
}

fn press_launcher(app: &mut App) {
    let launcher = app.root_widget.children_mut().into_iter().next().unwrap();
    assert!(launcher.perform_action(
        &crate::a11y::Action::Activate,
        &[],
        &mut crate::widget::Edits::lend(&mut app.state.doc),
        &app.state.root
    ));
}

#[test]
fn the_search_launcher_opens_this_windows_palette_narrowed_and_prefilled() {
    let mut app = mail_app(
        SearchRequest::within(ScopeDecl::new("folder:inbox", "Inbox")).with_query("invoice"),
    );
    assert!(app.action_palette().is_none());
    press_launcher(&mut app);
    assert!(app.dispatch_control_invocations() > 0);
    let palette = app.action_palette().expect("the launcher opens search");
    assert_eq!(
        palette.scope().map(|scope| scope.label.as_str()),
        Some("Inbox")
    );
    assert_eq!(palette.query(), "invoice");

    let palette = app.actions.action_palette.as_mut().unwrap();
    while palette.advance_search_page().pending {}
    let labels = palette.result_labels();
    assert!(
        labels.iter().any(|label| label == "Invoice from Ada"),
        "{labels:?}"
    );
    assert!(
        !labels.iter().any(|label| label == "Invoice to Grace"),
        "a hit outside the scope stays out: {labels:?}"
    );
    assert!(
        !labels.iter().any(|label| label.contains("Archive Sent")),
        "the actions offered are the scope's own: {labels:?}"
    );
}

#[test]
fn an_unscoped_palette_offers_each_scope_as_a_search_in_row() {
    let mut app = mail_app(SearchRequest::this_app());
    press_launcher(&mut app);
    assert!(app.dispatch_control_invocations() > 0);
    let palette = app.actions.action_palette.as_mut().unwrap();
    assert!(palette.scope().is_none());
    palette.set_query("sent");
    let labels = palette.result_labels();
    assert!(labels.iter().any(|label| label == "Sent"), "{labels:?}");
    let row = labels.iter().position(|label| label == "Sent").unwrap();
    palette.set_cursor(row);
    palette.activate_selected();
    assert_eq!(
        palette.scope().map(|scope| scope.anchor.as_str()),
        Some("folder:sent"),
        "picking the row narrows the search to it"
    );
    assert_eq!(
        palette.query(),
        "",
        "the search starts afresh inside the scope"
    );
}

#[cfg(feature = "native")]
#[test]
fn slash_opens_search_only_when_nothing_takes_the_text() {
    use winit::event::{ElementState, KeyEvent};
    use winit::keyboard::{Key, KeyLocation, NativeKeyCode, PhysicalKey};

    let slash = WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event: KeyEvent::synthetic(
            winit::event::RawKeyEvent {
                physical_key: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                state: ElementState::Pressed,
            },
            Key::Character("/".into()),
            Some("/".into()),
            KeyLocation::Standard,
            false,
            None,
            Key::Character("/".into()),
        ),
        is_synthetic: false,
    };
    let mut app = mail_app(SearchRequest::this_app());
    assert!(app.handle_unconsumed_search_key(&slash));
    assert!(app.action_palette().is_some());
    assert!(
        !app.handle_unconsumed_search_key(&slash),
        "an open palette types the slash itself"
    );

    // A window without its own search (hosted in the shell) leaves `/` to
    // the shell.
    let mut hosted = App::new_with_controlled_inputs(
        State::new(),
        Box::new(MailRoot {
            search: search_button(SearchRequest::this_app()),
        }),
    );
    hosted.init_layout();
    assert!(!hosted.handle_unconsumed_search_key(&slash));
    assert!(hosted.action_palette().is_none());
}

use super::*;
use crate::application::{LaunchTarget, RestoreInput};
use crate::application_test_support as probe;
use crate::search::{SearchProvider, SearchResult, search_button};
use crate::widget::{AppModel, Button, ModelPath, Widget};
use std::cell::RefCell;
use std::rc::Rc;

struct Messages;
impl SearchProvider for Messages {
    fn name(&self) -> &'static str {
        "Messages"
    }
    fn search(&self, query: &str) -> Vec<SearchResult> {
        [
            ("inbox/message:1", "Invoice from Ada"),
            ("sent/message:2", "Invoice to Grace"),
        ]
        .into_iter()
        .filter(|(_, title)| title.to_lowercase().contains(&query.to_lowercase()))
        .map(|(id, title)| SearchResult::new("Messages", title, id))
        .collect()
    }
    fn search_scopes(&self) -> Vec<ScopeDecl> {
        vec![
            ScopeDecl::new("inbox", "Inbox"),
            ScopeDecl::new("sent", "Sent"),
        ]
    }
}
struct Mail {
    button: Button,
    paged: bool,
    opened: Rc<RefCell<Vec<String>>>,
}
impl crate::widget::Decorator for Mail {
    type Inner = Button;
    fn inner(&self) -> &Button {
        &self.button
    }
    fn inner_mut(&mut self) -> &mut Button {
        &mut self.button
    }
    crate::provides!(dyn crate::search::capability::SearchSource; inner = button);
}
impl crate::search::capability::SearchSource for Mail {
    fn search_providers(&self) -> Vec<Box<dyn SearchProvider>> {
        if self.paged {
            vec![Box::new(PagedMessages)]
        } else {
            vec![Box::new(Messages)]
        }
    }
    fn activate_search_result(&mut self, id: &str, _: &mut AppModel, _: &ModelPath) {
        self.opened.borrow_mut().push(id.into());
    }
}

#[test]
fn launcher_opens_scoped_native_palette_and_navigates_its_provider() {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let root = Mail {
        paged: false,
        button: search_button(
            SearchRequest::within(ScopeDecl::new("inbox", "Inbox")).with_query("invoice"),
        ),
        opened: opened.clone(),
    };
    let mut host =
        FrameHarness::for_root(crate::State::new(), root, (800, 600), crate::Theme::dark());
    let rect = host.root::<Mail>().button.get_rect();
    host.click_at(rect.center().x, rect.center().y);
    assert_eq!(host.app_search_query(), Some("invoice"));
    assert_eq!(
        host.app_search_scope(),
        Some(&ScopeDecl::new("inbox", "Inbox"))
    );
    assert!(host.global_search_query().is_none());
    assert!(
        host.open_app_search(SearchRequest::this_app().with_query("other"))
            .is_err()
    );
    assert_eq!(host.app_search_query(), Some("invoice"));
    let labels = host.action_palette.as_ref().unwrap().result_labels();
    assert!(labels.iter().any(|s| s == "Invoice from Ada"), "{labels:?}");
    assert!(
        !labels.iter().any(|s| s == "Invoice to Grace"),
        "{labels:?}"
    );
    host.key("Enter");
    assert_eq!(*opened.borrow(), ["inbox/message:1"]);
    assert!(host.app_search_query().is_none());
}

#[test]
fn standalone_component_search_uses_provider_and_exact_action_dispatch() {
    let fixture = probe::ProbeHost::new();
    let observed = probe::enable_provider_probe();
    let definition = probe::definition(probe::action_probe_factory);
    let context = fixture
        .prepare_with_permissions(
            &definition,
            7,
            &LaunchTarget::New,
            probe::runner_permissions(),
        )
        .unwrap();
    let component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let binding = component.callback_binding();
    let mut host =
        FrameHarness::from_component(component, (800, 600), probe::runner_inputs()).unwrap();
    binding
        .queued_controls
        .borrow_mut()
        .push_back(crate::search::open_search_invocation(
            &SearchRequest::this_app().with_query("View"),
        ));
    host.host_tick();
    host.relayout();
    assert_eq!(host.app_search_query(), Some("View"));
    assert!(host.app_search_scope().is_none());
    assert!(binding.take_search_request().is_none());
    assert!(
        observed
            .borrow()
            .methods
            .iter()
            .any(|(method, active)| *method == "page" && *active)
    );
    host.key("Enter");
    assert_eq!(*probe::operation_observations()[0].borrow(), ["View"]);
    assert!(host.app_search_query().is_none());
}

#[test]
fn embedded_search_stays_with_the_captured_component_and_drains_once() {
    let fixture = probe::ProbeHost::new();
    let first =
        probe::instantiate_probe(&fixture, 11, LaunchTarget::New, RestoreInput::None).unwrap();
    let second =
        probe::instantiate_probe(&fixture, 12, LaunchTarget::New, RestoreInput::None).unwrap();
    let a = first.callback_binding();
    let b = second.callback_binding();
    let root = crate::widget::Column::new()
        .push(Box::new(crate::widget::BoundAppWidget { component: first }))
        .push(Box::new(crate::widget::BoundAppWidget {
            component: second,
        }));
    let mut host =
        FrameHarness::for_root(crate::State::new(), root, (800, 600), crate::Theme::dark());
    let request = SearchRequest::within(ScopeDecl::new("inbox", "Inbox")).with_query("invoice");
    a.queued_controls
        .borrow_mut()
        .push_back(crate::search::open_search_invocation(&request));
    host.host_tick();
    assert!(host.app_search_query().is_none());
    assert_eq!(a.take_search_request(), Some(request));
    assert!(a.take_search_request().is_none());
    assert!(b.take_search_request().is_none());
}

struct Nested(crate::widget::Column);
impl crate::widget::Decorator for Nested {
    type Inner = crate::widget::Column;
    fn inner(&self) -> &Self::Inner {
        &self.0
    }
    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.0
    }
}
crate::testing::test_app_widget!(Nested, "Nested apps");

#[test]
fn standalone_parent_does_not_steal_nested_components_request() {
    let fixture = probe::ProbeHost::new();
    let mut parent =
        probe::instantiate_probe(&fixture, 1, LaunchTarget::New, RestoreInput::None).unwrap();
    let child =
        probe::instantiate_probe(&fixture, 2, LaunchTarget::New, RestoreInput::None).unwrap();
    let parent_binding = parent.callback_binding();
    let child_binding = child.callback_binding();
    parent.root = Some(Box::new(Nested(
        crate::widget::Column::new()
            .push(parent.root.take().unwrap())
            .push(Box::new(crate::widget::BoundAppWidget { component: child })),
    )));
    let mut host =
        FrameHarness::from_component(parent, (800, 600), probe::runner_inputs()).unwrap();
    let request = SearchRequest::this_app().with_query("child only");
    child_binding
        .queued_controls
        .borrow_mut()
        .push_back(crate::search::open_search_invocation(&request));
    host.host_tick();
    assert!(host.app_search_query().is_none());
    assert_eq!(child_binding.take_search_request(), Some(request));
    assert!(parent_binding.take_search_request().is_none());
}

#[test]
fn search_row_confirmation_captures_contract_before_later_submit() {
    for changed in [false, true] {
        let (root, ledger, resources) = crate::app::ordinary_confirmation_tests::fixture(
            tilo_actions::ConfirmationPolicy::Confirm,
        );
        let mut host = FrameHarness::from_managed_root_with_controlled_inputs(
            crate::State::new(),
            root,
            (900, 650),
            crate::Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("fixture".into()),
        )
        .unwrap();
        host.open_app_search(SearchRequest::this_app().with_query("Run fixture"))
            .unwrap();
        host.relayout();
        host.key("Enter");
        // The typed operation asks for its single declared text argument.
        host.type_text("from search");
        host.key("Enter");
        let field = host.a11y_node_named("Confirmation").unwrap();
        host.perform_a11y_action(field, &crate::a11y::Action::SetValue("CONFIRM".into()))
            .unwrap();
        if changed {
            resources.borrow_mut()[0].actions[0].label = "Changed meaning".into();
        }
        let submit = host.a11y_node_named("Confirm").unwrap();
        host.perform_a11y_action(submit, &crate::a11y::Action::Activate)
            .unwrap();
        if changed {
            assert!(ledger.borrow().is_empty());
        } else {
            assert_eq!(
                *ledger.borrow(),
                [serde_json::json!({"payload":"from search"})]
            );
        }
    }
}

struct PagedMessages;
impl SearchProvider for PagedMessages {
    fn name(&self) -> &'static str {
        "Paged messages"
    }
    fn search(&self, _: &str) -> Vec<SearchResult> {
        panic!("host must use bounded pages")
    }
    fn search_page(
        &self,
        query: &crate::search::SearchQuery,
        cursor: Option<&crate::search::SearchCursor>,
        work: usize,
        limit: usize,
    ) -> crate::search::SearchPage {
        assert!(work > 0 && limit > 0);
        let page = cursor.and_then(|c| c.opaque_parts().3).unwrap_or(0);
        crate::search::SearchPage {
            results: if page == 12 {
                vec![SearchResult::new("Messages", "Late invoice", "late:12")]
            } else {
                vec![]
            },
            next: (page < 12).then(|| {
                crate::search::SearchCursor::opaque(1, query.text(), None, Some(page + 1))
            }),
            inspected: 1,
            pending: None,
        }
    }
}

#[test]
fn provider_continuations_advance_through_real_frames_and_stop_scheduling() {
    let root = Mail {
        button: search_button(SearchRequest::this_app().with_query("invoice")),
        opened: Rc::default(),
        paged: true,
    };
    let mut host =
        FrameHarness::for_root(crate::State::new(), root, (800, 600), crate::Theme::dark());
    let rect = host.root::<Mail>().button.get_rect();
    host.click_at(rect.center().x, rect.center().y);
    assert!(
        host.frame_schedule().queued_work,
        "incomplete provider pages request another frame"
    );
    let mut frames = 0;
    while host.frame_schedule().queued_work && frames < 64 {
        assert!(
            host.poll_frame().unwrap(),
            "queued work produces a scheduled frame"
        );
        frames += 1;
    }
    assert!(
        frames > 0 && frames < 64,
        "provider pages finish within bounded scheduled frames"
    );
    assert!(
        host.a11y_nodes()
            .iter()
            .any(|(_, node)| node.label() == Some("Late invoice")),
        "scheduled poll_frame reaches the later page"
    );
    let _ = host.poll_frame().unwrap();
    assert!(
        !host.frame_schedule().queued_work,
        "finished pages do not create an endless redraw loop"
    );
}

//! Actions of private windows, offered to this device only.
//!
//! A private window (the feedback composer) has no collaboration route: what
//! it holds must not reach a room or another device. Its door's actions are
//! still the way a script, an agent on this desktop or a screen reader drives
//! it, so the overlay host reads what each private window publishes here
//! ([`DeviceLocalWindow`]) and this provider lists those actions in the
//! desktop's own catalogue. An invocation is queued for the overlay host,
//! which runs it through the window's door as the window's own person.
//! Nothing here is published to a room or answered for a remote device.

use std::cell::RefCell;
use std::rc::Rc;

use crate::shell::chrome_state::Chrome;
use crate::shell::search::action_api::{ActionArgs, ActionError};
use crate::shell::search::{
    ActionReceipt, InvocationDisposition, PendingInvocation, ResourceKind, SearchContext,
    SearchProvider, SearchResult,
};

pub const PROVIDER_ID: &str = "device-local";
const ID_SEPARATOR: char = '\u{1f}';

/// What one private window publishes now, as the overlay host last read it.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceLocalWindow {
    pub tab: u64,
    pub actions: Vec<tilo_actions::ActionDescriptor>,
}

/// The answer the overlay host writes for a queued invocation.
pub type Answer = Rc<RefCell<Option<Result<ActionReceipt, ActionError>>>>;

/// An invocation waiting for the overlay host, which owns the windows.
#[derive(Debug)]
pub struct DeviceLocalInvocation {
    pub tab: u64,
    pub address: tilo_actions::ActionAddress,
    pub arguments: serde_json::Map<String, serde_json::Value>,
    pub answer: Answer,
}

/// The receipt an invocation the window's door started answers with.
///
/// # Errors
/// The door's refusal, as the catalogue reports it.
pub fn receipt(
    started: Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError>,
) -> Result<ActionReceipt, ActionError> {
    let invocation = tilo_actions::InvocationId::new();
    match started {
        Ok(ui::host_operation::ResourceOperationStart::Completed(output)) => Ok(ActionReceipt {
            broker_invocation_id: None,
            update: tilo_actions::InvocationUpdate::Succeeded { invocation, output },
        }),
        Ok(ui::host_operation::ResourceOperationStart::Accepted) => Ok(ActionReceipt {
            broker_invocation_id: None,
            update: tilo_actions::InvocationUpdate::Accepted { invocation },
        }),
        Err(error) => Err(ActionError::Refused(error.to_string())),
    }
}

/// The catalogue of private windows' actions.
#[derive(Default)]
pub struct DeviceLocalActionsProvider;

pub(crate) fn row_id(tab: u64, address: &tilo_actions::ActionAddress) -> String {
    format!(
        "{tab}{ID_SEPARATOR}{}{ID_SEPARATOR}{}",
        address.resource, address.action
    )
}

/// Queue `address` for the overlay host and answer once it ran.
fn begin(
    chrome: &Chrome,
    tab: u64,
    address: &tilo_actions::ActionAddress,
    args: &ActionArgs,
) -> PendingInvocation {
    let answer: Answer = Rc::default();
    {
        let mut state = chrome.borrow_mut();
        state.device_local_invocations.push(DeviceLocalInvocation {
            tab,
            address: address.clone(),
            arguments: args.to_json(),
            answer: Rc::clone(&answer),
        });
        state.bump();
    }
    Rc::new(move || answer.borrow_mut().take().transpose())
}

impl SearchProvider for DeviceLocalActionsProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }
    fn name(&self) -> &'static str {
        "Private windows"
    }
    fn display_name(&self) -> String {
        crate::shell::i18n::t("search-provider-device-local")
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        ctx.chrome.device_local_revision.to_string()
    }
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return Vec::new();
        }
        let needle = ctx.needle.trim().to_lowercase();
        let mut rows = Vec::new();
        for window in &ctx.chrome.device_local_windows {
            for descriptor in &window.actions {
                let title = format!("{}: {}", descriptor.resource_label, descriptor.action_label);
                if !needle.is_empty()
                    && !PROVIDER_ID.contains(&needle)
                    && !title.to_lowercase().contains(&needle)
                    && !descriptor.address.action.contains(&needle)
                {
                    continue;
                }
                let tab = window.tab;
                let address = descriptor.address.clone();
                let open_address = address.clone();
                rows.push(SearchResult {
                    facts: Vec::new(),
                    kind: ResourceKind::Action,
                    operation_kind: descriptor.kind,
                    provider_id: PROVIDER_ID.into(),
                    id: row_id(tab, &address),
                    title,
                    subtitle: descriptor.description.clone(),
                    badge: None,
                    shortcut: None,
                    score: if needle.is_empty() { 10 } else { 50 },
                    params: descriptor.params.clone(),
                    contract: Some(descriptor.contract.clone()),
                    on_open: Rc::new(move |_, _, chrome, args| {
                        drop(begin(chrome, tab, &open_address, args));
                        Ok(())
                    }),
                    on_invoke: None,
                    on_begin_invoke: Some(Rc::new(move |_, _, chrome, args| {
                        Ok(InvocationDisposition::Pending(begin(
                            chrome, tab, &address, args,
                        )))
                    })),
                });
            }
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_name_their_window_and_action_and_queue_for_the_overlay_host() {
        let chrome = crate::shell::chrome_state::ChromeState::new(
            crate::shell::state::WorkspaceTree::bootstrap_default(),
        );
        let resource = tilo_actions::Resource::new("draft", "tilo.feedback-draft", "Feedback")
            .with_action(tilo_actions::ActionSpec::immediate_encoded(
                "review",
                "Review",
                tilo_actions::ExecutionLocus::ClientLocal,
                tilo_actions::EncodedAction {
                    type_id: "fixture.review".into(),
                    payload: Vec::new(),
                },
            ));
        chrome.borrow_mut().device_local_windows = vec![DeviceLocalWindow {
            tab: 7,
            actions: tilo_actions::door::survey(&[resource]),
        }];
        let doc = automerge::AutoCommit::new();
        let state = chrome.borrow();
        let rows = DeviceLocalActionsProvider.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "review",
            wanted: &[ResourceKind::Action],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        drop(state);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].id,
            format!("7{ID_SEPARATOR}draft{ID_SEPARATOR}review")
        );
        let begin = rows[0].on_begin_invoke.clone().unwrap();
        let mut doc = automerge::AutoCommit::new();
        let Ok(InvocationDisposition::Pending(ticket)) = begin(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &ActionArgs::none(),
        ) else {
            panic!("a private window's action waits for the overlay host");
        };
        assert!(matches!(ticket(), Ok(None)));
        let queued = std::mem::take(&mut chrome.borrow_mut().device_local_invocations);
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].tab, 7);
        *queued[0].answer.borrow_mut() = Some(receipt(Ok(
            ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ),
        )));
        assert!(matches!(ticket(), Ok(Some(_))));
    }
}

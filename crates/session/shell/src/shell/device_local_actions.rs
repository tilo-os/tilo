//! Actions of private windows, offered to this device only.
//!
//! A private window (the feedback composer, Quick Settings) has no
//! collaboration route: what it holds must not reach a room or another
//! device. Its door's actions are still the way a person searching, a
//! script, an agent on this desktop or a screen reader drives it, so the
//! overlay host reads what each private window publishes here
//! ([`DeviceLocalWindow`]) and this provider lists those actions in the
//! desktop's own catalogue, found by their labels and the words their app
//! declares for them as the shell's own actions are found by theirs. An
//! invocation is queued for the overlay host, which runs it through the
//! window's door as the window's own person. Nothing here is published to a
//! room or answered for a remote device.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use ui::search::{FieldWeight, ListIndex};

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
pub struct DeviceLocalActionsProvider {
    /// Ranks each action by its label, the words its app declares for it,
    /// what it acts on and its name, as the shell's own actions rank.
    ranker: ListIndex,
    /// What each action offered at the last query was found by and wears,
    /// by its row's id.
    offered: RefCell<HashMap<String, Offered>>,
}

/// One action as the last query offered it.
struct Offered {
    fields: Vec<(String, FieldWeight)>,
    icon: Option<ui::icons::Icon>,
}

impl Default for DeviceLocalActionsProvider {
    fn default() -> Self {
        Self {
            ranker: ListIndex::default().with_ranks(ui::search::system::action_ranks),
            offered: RefCell::default(),
        }
    }
}

pub(crate) fn row_id(tab: u64, address: &tilo_actions::ActionAddress) -> String {
    format!(
        "{tab}{ID_SEPARATOR}{}{ID_SEPARATOR}{}",
        address.resource, address.action
    )
}

/// What `descriptor` is found by: its label, what it acts on, its name and
/// the other words its app declares for it.
fn found_by(descriptor: &tilo_actions::ActionDescriptor) -> Vec<(String, FieldWeight)> {
    let mut fields = vec![
        (descriptor.action_label.clone(), FieldWeight::TITLE),
        (descriptor.resource_label.clone(), FieldWeight::SUBTITLE),
        (descriptor.address.action.clone(), FieldWeight::IDENTITY),
    ];
    fields.extend(
        descriptor
            .keywords
            .iter()
            .map(|word| (word.clone(), FieldWeight::KEYWORD)),
    );
    fields
}

/// The row of `descriptor`, published by the private window `tab`.
fn row(tab: u64, descriptor: &tilo_actions::ActionDescriptor, score: i32) -> SearchResult {
    let address = descriptor.address.clone();
    let open_address = address.clone();
    SearchResult {
        facts: Vec::new(),
        kind: ResourceKind::Action,
        operation_kind: descriptor.kind,
        provider_id: PROVIDER_ID.into(),
        id: row_id(tab, &address),
        // The action as its app names it, and what it acts on; never the
        // description written for agents and scripts.
        title: descriptor.action_label.clone(),
        subtitle: Some(descriptor.resource_label.clone()),
        badge: None,
        shortcut: None,
        score,
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
        resource: None,
    }
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
        let listed: Vec<(u64, &tilo_actions::ActionDescriptor)> = ctx
            .chrome
            .device_local_windows
            .iter()
            .flat_map(|window| {
                window
                    .actions
                    .iter()
                    .map(move |descriptor| (window.tab, descriptor))
            })
            .filter(|(_, descriptor)| {
                ctx.include_programmatic || descriptor.contract.reaches_people()
            })
            .collect();
        let searched: Vec<Vec<(String, FieldWeight)>> = listed
            .iter()
            .map(|(_, descriptor)| found_by(descriptor))
            .collect();
        // Naming the catalogue itself lists all of it.
        let needle = ctx.needle.trim();
        let needle = if needle.eq_ignore_ascii_case(PROVIDER_ID) {
            ""
        } else {
            needle
        };
        let ranks = self.ranker.ranks(
            needle,
            searched
                .iter()
                .map(|fields| fields.iter().map(|(text, weight)| (text.as_str(), *weight))),
        );
        let mut offered = HashMap::new();
        let mut rows = Vec::new();
        for (((tab, descriptor), fields), rank) in listed.into_iter().zip(searched).zip(ranks) {
            let id = row_id(tab, &descriptor.address);
            if let Some(rank) = rank {
                rows.push(row(
                    tab,
                    descriptor,
                    crate::shell::search::provider_score(rank),
                ));
            }
            let icon = descriptor
                .icon
                .as_deref()
                .and_then(ui::icons::Icon::from_name);
            offered.insert(id, Offered { fields, icon });
        }
        *self.offered.borrow_mut() = offered;
        // Equal matches: what does it in one step before what asks for
        // input, so "wifi" switches Wi-Fi ahead of asking whether to.
        crate::shell::search::usage::order_ties_by(&mut rows, |row| {
            i32::from(row.params.is_empty())
        });
        rows
    }

    /// An address is looked up among every window's actions, whatever a
    /// query would rank.
    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return None;
        }
        ctx.chrome
            .device_local_windows
            .iter()
            .find_map(|window| {
                window
                    .actions
                    .iter()
                    .find(|descriptor| row_id(window.tab, &descriptor.address) == id)
                    .map(|descriptor| (window.tab, descriptor))
            })
            .map(|(tab, descriptor)| row(tab, descriptor, crate::shell::search::provider_score(0)))
    }

    /// An action that cannot be used now (a control this device lacks)
    /// only takes a row from one that can: it is offered to a person only
    /// when the query names it exactly, where choosing it says why not.
    fn hidden_from_people(
        &self,
        result: &SearchResult,
        needle: &str,
        _chrome: &crate::shell::chrome_state::ChromeState,
    ) -> bool {
        result
            .contract
            .as_ref()
            .is_some_and(|contract| !contract.availability.is_available())
            && tilo_search::fold(needle.trim()) != tilo_search::fold(&result.title)
    }

    fn row_icon(&self, result: &SearchResult) -> Option<ui::icons::Icon> {
        self.offered.borrow().get(&result.id)?.icon
    }

    fn match_reason(&self, result: &SearchResult, needle: &str) -> Option<String> {
        let offered = self.offered.borrow();
        crate::shell::search::match_reason_in(needle, result, &offered.get(&result.id)?.fields)
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
        let resource = tilo_actions::ResourceSpec::new("draft", "tilo.feedback-draft", "Feedback")
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
        let rows = DeviceLocalActionsProvider::default().query(SearchContext {
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

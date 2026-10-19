//! "Run shortcut ▸": the person's shortcuts, offered on a file.
//!
//! Files asks the session's action host which shortcuts the person may run
//! (the `shortcuts.shortcut:*` rows of its catalogue) and offers on a
//! selected entry those whose first input takes that entry's kind of
//! resource ([`tilo_automation::quick_action_input`]). Choosing one invokes
//! that shortcut's `run` through the same door every other caller uses, with
//! the entry's resource id bound to that input, so the run meets the same
//! checks as one from the palette or an agent.

use ui::time::{Duration, Instant};

use tilo_actions::remote::{
    CatalogueQuery, InvokeArgument, InvokeRequest, MAX_CATALOGUE_PAGE, ReceiptStatus,
    RemoteActionMessage, RemoteActionTarget,
};
use tilo_actions::{ActionDescriptor, InvocationId};
use tilo_automation::{
    SHORTCUT_RESOURCE_KIND, SHORTCUT_RUN_ACTION, SHORTCUTS_APP_ID, quick_action_arguments,
    quick_action_input,
};
use ui::collaboration::{CollabEvent, CollabRequest};

/// How long a fetched list of shortcuts is offered before it is asked for
/// again.
const REFRESH_AFTER: Duration = Duration::from_secs(15);

/// Where Files numbers its own catalogue requests, far from the toolkit's.
const CORRELATION_BASE: u64 = 0x5107_c075_0000_0000;

/// One shortcut the person may run, as the action host offered it.
#[derive(Clone, Debug, PartialEq)]
pub struct ShortcutOffer {
    pub provider: String,
    pub descriptor: ActionDescriptor,
}

impl ShortcutOffer {
    /// The shortcut's resource id, which names it.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.descriptor.address.resource
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.descriptor.action_label
    }

    /// The resource of `identities` (kind and id) this shortcut runs on,
    /// when it runs on one of them.
    #[must_use]
    pub fn resource_in<'r>(&self, identities: &'r [(String, String)]) -> Option<&'r str> {
        identities
            .iter()
            .find(|(kind, _)| quick_action_input(&self.descriptor.params, kind).is_some())
            .map(|(_, resource)| resource.as_str())
    }
}

/// What became of a shortcut run from the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShortcutOutcome {
    Started { label: String },
    Refused { label: String, reason: String },
}

/// The shortcuts offered in Files, and the runs started from its menu.
#[derive(Debug, Default)]
pub struct ShortcutMenuState {
    offers: Vec<ShortcutOffer>,
    gathering: Vec<ShortcutOffer>,
    requests: Vec<CollabRequest>,
    issued: u64,
    pending: Option<u64>,
    refreshed: Option<Instant>,
    running: Vec<(InvocationId, String)>,
}

impl ShortcutMenuState {
    /// Ask for the shortcuts again when the list is older than
    /// [`REFRESH_AFTER`] and no answer is outstanding.
    pub fn poll_if_due(&mut self, now: Instant) {
        let stale = self
            .refreshed
            .is_none_or(|refreshed| now.saturating_duration_since(refreshed) >= REFRESH_AFTER);
        if stale && self.pending.is_none() {
            self.refreshed = Some(now);
            self.gathering.clear();
            self.request_page(Vec::new());
        }
    }

    fn request_page(&mut self, cursor: Vec<u8>) {
        self.issued = self.issued.wrapping_add(1);
        let correlation = CORRELATION_BASE.wrapping_add(self.issued);
        self.pending = Some(correlation);
        self.requests.push(CollabRequest::RemoteAction {
            message: RemoteActionMessage::CatalogueRequest {
                correlation,
                query: CatalogueQuery {
                    needle: SHORTCUT_RESOURCE_KIND.into(),
                    cursor,
                    max_results: MAX_CATALOGUE_PAGE,
                    ..CatalogueQuery::default()
                },
            },
        });
    }

    /// Requests for the session, oldest first.
    pub fn take_requests(&mut self) -> Vec<CollabRequest> {
        std::mem::take(&mut self.requests)
    }

    /// Take the session's answer to a request made here; what became of a
    /// run, when `event` says.
    pub fn handle_event(&mut self, event: &CollabEvent) -> Option<ShortcutOutcome> {
        let CollabEvent::RemoteAction { message } = event else {
            return None;
        };
        match message {
            RemoteActionMessage::CatalogueResponse {
                correlation,
                result,
            } if Some(*correlation) == self.pending => {
                self.pending = None;
                let Ok(page) = result else {
                    self.gathering.clear();
                    return None;
                };
                if page.stale_cursor {
                    self.gathering.clear();
                    self.request_page(Vec::new());
                    return None;
                }
                self.gathering.extend(page.rows.iter().filter_map(|row| {
                    let descriptor = row.action.as_ref()?;
                    (row.app_id == SHORTCUTS_APP_ID
                        && descriptor.address.action == SHORTCUT_RUN_ACTION)
                        .then(|| ShortcutOffer {
                            provider: row.provider.clone(),
                            descriptor: descriptor.clone(),
                        })
                }));
                if page.next_cursor.is_empty() {
                    self.offers = std::mem::take(&mut self.gathering);
                } else {
                    self.request_page(page.next_cursor.clone());
                }
                None
            }
            RemoteActionMessage::Receipt(receipt) => {
                let at = self
                    .running
                    .iter()
                    .position(|(invocation, _)| *invocation == receipt.invocation)?;
                let (_, label) = self.running.remove(at);
                Some(match &receipt.status {
                    ReceiptStatus::Rejected { error } => ShortcutOutcome::Refused {
                        label,
                        reason: error.to_string(),
                    },
                    ReceiptStatus::Failed { error } => ShortcutOutcome::Refused {
                        label,
                        reason: error.message.clone(),
                    },
                    ReceiptStatus::Accepted
                    | ReceiptStatus::Completed { .. }
                    | ReceiptStatus::Cancelled { .. } => ShortcutOutcome::Started { label },
                })
            }
            _ => None,
        }
    }

    /// The shortcuts that run on a resource with one of `identities` (kind
    /// and id), in the order the host offered them.
    #[must_use]
    pub fn offered_on(&self, identities: &[(String, String)]) -> Vec<&ShortcutOffer> {
        self.offers
            .iter()
            .filter(|offer| offer.resource_in(identities).is_some())
            .collect()
    }

    /// Run shortcut `shortcut` on the resource of `identities` it takes.
    ///
    /// # Errors
    ///
    /// Why it cannot run on that resource, for the status line.
    pub fn run(
        &mut self,
        shortcut: &str,
        identities: &[(String, String)],
    ) -> Result<String, String> {
        let offer = self
            .offers
            .iter()
            .find(|offer| offer.id() == shortcut)
            .ok_or_else(|| "that shortcut is no longer offered".to_string())?;
        let (kind, resource) = identities
            .iter()
            .find(|(kind, _)| quick_action_input(&offer.descriptor.params, kind).is_some())
            .ok_or_else(|| format!("“{}” does not run on this item", offer.label()))?;
        let input = quick_action_input(&offer.descriptor.params, kind)
            .ok_or_else(|| format!("“{}” does not run on this item", offer.label()))?;
        let invocation = InvocationId::new();
        let label = offer.label().to_string();
        self.requests.push(CollabRequest::RemoteAction {
            message: RemoteActionMessage::Invoke(InvokeRequest {
                on_behalf_of: None,
                invocation,
                target: RemoteActionTarget {
                    provider: offer.provider.clone(),
                    address: offer.descriptor.address.clone(),
                },
                argument: InvokeArgument::Json(quick_action_arguments(input, resource)),
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
            }),
        });
        self.running.push((invocation, label.clone()));
        Ok(label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::params::{ActionParam, ParamType};
    use tilo_actions::remote::{CataloguePage, CatalogueResource, CatalogueRow, InvocationReceipt};
    use tilo_actions::{ActionContract, ExecutionLocus, InputSpec, OperationKind};

    fn shortcut_row(id: &str, label: &str, params: Vec<ActionParam>) -> CatalogueRow {
        let address = tilo_automation::shortcut_run_address(id);
        let resource = address.resource.clone();
        CatalogueRow {
            provider: "workspace:room".into(),
            app_id: SHORTCUTS_APP_ID.into(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: resource.clone(),
                kind: SHORTCUT_RESOURCE_KIND.into(),
                label: label.into(),
                presentation: None,
            },
            action: Some(ActionDescriptor {
                semantic_identity: None,
                address,
                resource_kind: SHORTCUT_RESOURCE_KIND.into(),
                resource_label: label.into(),
                action_label: label.into(),
                description: None,
                locus: ExecutionLocus::default(),
                kind: OperationKind::View,
                contract: ActionContract::view(),
                input: InputSpec::None,
                params,
                view: None,
            }),
            has_children: false,
            visibility: None,
        }
    }

    /// A menu that has fetched a shortcut on files, one on calendar events
    /// and one that takes nothing.
    fn fetched() -> ShortcutMenuState {
        let mut menu = ShortcutMenuState::default();
        menu.poll_if_due(ui::time::now());
        let requests = menu.take_requests();
        let [
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::CatalogueRequest { correlation, query },
            },
        ] = requests.as_slice()
        else {
            panic!("one catalogue request: {requests:?}");
        };
        assert_eq!(query.needle, SHORTCUT_RESOURCE_KIND);
        let page = CataloguePage {
            rows: vec![
                shortcut_row(
                    "archive",
                    "Archive",
                    vec![
                        ActionParam::required("file", ParamType::Resource, "The file")
                            .of_resource_kind("files.entry"),
                    ],
                ),
                shortcut_row(
                    "reschedule",
                    "Reschedule",
                    vec![
                        ActionParam::required("event", ParamType::Resource, "The event")
                            .of_resource_kind("calendar.event"),
                    ],
                ),
                shortcut_row("tidy", "Tidy", Vec::new()),
            ],
            exhausted: true,
            ..CataloguePage::default()
        };
        assert!(
            menu.handle_event(&CollabEvent::RemoteAction {
                message: RemoteActionMessage::CatalogueResponse {
                    correlation: *correlation,
                    result: Ok(page),
                },
            })
            .is_none()
        );
        menu
    }

    fn file() -> Vec<(String, String)> {
        vec![("files.entry".into(), "files.entry:0123".into())]
    }

    #[test]
    fn a_file_is_offered_the_shortcuts_that_run_on_files_only() {
        let menu = fetched();
        let offered: Vec<&str> = menu
            .offered_on(&file())
            .into_iter()
            .map(ShortcutOffer::label)
            .collect();
        assert_eq!(offered, ["Archive"]);
        assert!(
            menu.offered_on(&[("files.record".into(), "files.record:1".into())])
                .is_empty()
        );
    }

    #[test]
    fn running_a_shortcut_on_a_file_binds_the_file_to_its_first_input() {
        let mut menu = fetched();
        assert!(menu.run("shortcuts.shortcut:reschedule", &file()).is_err());
        assert_eq!(
            menu.run("shortcuts.shortcut:archive", &file()),
            Ok("Archive".into())
        );
        let requests = menu.take_requests();
        let [
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
            },
        ] = requests.as_slice()
        else {
            panic!("one invocation: {requests:?}");
        };
        assert_eq!(request.target.provider, "workspace:room");
        assert_eq!(
            request.target.address,
            tilo_automation::shortcut_run_address("archive")
        );
        assert_eq!(
            request.argument,
            InvokeArgument::Json(
                serde_json::json!({ "file": "files.entry:0123" })
                    .as_object()
                    .cloned()
                    .expect("an object")
            )
        );
        assert_eq!(
            menu.handle_event(&CollabEvent::RemoteAction {
                message: RemoteActionMessage::Receipt(InvocationReceipt {
                    invocation: request.invocation,
                    status: ReceiptStatus::Accepted,
                }),
            }),
            Some(ShortcutOutcome::Started {
                label: "Archive".into()
            })
        );
    }
}

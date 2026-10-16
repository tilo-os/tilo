//! Exact published Chat actions through the retained certified Person door.
use super::PersonDraftOwner;
use crate::shell::certified_action_session::DesktopActionSession;
use crate::shell::search::invocation_wake::{poll, waker, watch};
use std::time::{Duration, Instant};
use tilo_actions::remote::{
    ActionHostError, CanonicalActionRoute, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvokeArgument, InvokeRequest, Pending, PendingPoll, ReceiptStatus,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};

pub(super) enum Phase {
    Provider(Pending<Result<CataloguePage, ActionHostError>>),
    Resolve {
        provider: String,
        answer: Pending<Result<CataloguePage, ActionHostError>>,
    },
    Root {
        provider: String,
        revision: String,
        answer: Pending<Result<CataloguePage, ActionHostError>>,
    },
    Invoke {
        id: InvocationId,
        answer: Pending<Result<InvocationReceipt, ActionHostError>>,
    },
    Observe {
        id: InvocationId,
        change: Pending<()>,
    },
}

enum Step {
    Waiting,
    Next(Phase),
    Complete(Option<serde_json::Value>),
    Refused,
}

pub(super) struct InputContext {
    pub generation: u64,
    pub composition_generation: u64,
    pub editor: ui::private_draft::Editor,
}

pub(super) struct Handoff {
    pub token: uuid::Uuid,
    pub source: InputContext,
}

pub(super) struct Call {
    pub owner: PersonDraftOwner,
    pub text: Option<String>,
    pub handoff: Option<Handoff>,
    pub deadline: Instant,
    session: DesktopActionSession,
    phase: Phase,
}

impl Call {
    pub fn new(
        session: DesktopActionSession,
        owner: PersonDraftOwner,
        text: Option<String>,
    ) -> Self {
        let answer = watch(session.query(CatalogueQuery {
            canonical_route: Some(CanonicalActionRoute {
                room: owner.workspace.clone(),
                app_id: "so.tilo.chat".into(),
                record_id: String::new(),
            }),
            max_results: 0,
            ..CatalogueQuery::default()
        }));
        let now = Instant::now();
        Self {
            owner,
            text,
            handoff: None,
            deadline: now.checked_add(Duration::from_secs(30)).unwrap_or(now),
            session,
            phase: Phase::Provider(answer),
        }
    }

    pub fn expand(
        session: DesktopActionSession,
        owner: PersonDraftOwner,
        source: InputContext,
    ) -> Result<Self, String> {
        let token = session.stage_private_draft(ui::private_draft::Draft {
            workspace: owner.workspace.clone(),
            target: ui::private_draft::Target::Person {
                public_key: owner.person_key.clone(),
            },
            editor: source.editor.clone(),
        })?;
        let mut call = Self::new(session, owner, None);
        call.handoff = Some(Handoff { token, source });
        Ok(call)
    }

    /// None waits for a host event; true means owner-completed, never admission.
    pub fn pump(&mut self) -> Option<bool> {
        if Instant::now() >= self.deadline {
            return Some(false);
        }
        let address = ActionAddress::new(
            format!("chat.person:{}", self.owner.person_key),
            if self.text.is_some() {
                "send-direct-message"
            } else {
                "open-direct-conversation"
            },
        );
        // At most provider, contact proof, root resolution, receipt and one change delivery.
        // No idle refresh or recursive loop can spin on a nonterminal update.
        for _ in 0..5 {
            let observing = matches!(self.phase, Phase::Observe { .. });
            let step = match &mut self.phase {
                Phase::Provider(answer) => {
                    provider_step(&self.session, &self.owner, &address, answer)
                }
                Phase::Resolve { provider, answer } => resolve_step(
                    &self.session,
                    &address,
                    provider,
                    answer,
                    self.handoff.is_some(),
                ),
                Phase::Root {
                    provider,
                    revision,
                    answer,
                } => root_step(
                    &self.session,
                    &self.owner,
                    self.text.as_deref(),
                    self.handoff.as_ref().map(|handoff| handoff.token),
                    (provider, revision),
                    answer,
                ),
                Phase::Invoke { id, answer } => invoke_step(&self.session, *id, answer),
                Phase::Observe { id, change } => observe_step(&self.session, *id, change),
            };
            match step {
                Step::Waiting => return None,
                Step::Refused => return Some(false),
                Step::Complete(value) => {
                    return Some(if let Some(Handoff { token, .. }) = &self.handoff {
                        value.is_some_and(|v| {
                            v.get("adopted").and_then(serde_json::Value::as_str)
                                == Some(token.to_string().as_str())
                        })
                    } else {
                        self.text.is_none()
                            || value.is_some_and(|v| {
                                v.get("peer").and_then(serde_json::Value::as_str)
                                    == Some(self.owner.person_key.as_str())
                                    && v.get("accepted").and_then(serde_json::Value::as_bool)
                                        == Some(true)
                            })
                    });
                }
                Step::Next(phase) => self.phase = phase,
            }
            if observing {
                return None;
            }
        }
        None
    }
}

fn provider_step(
    session: &DesktopActionSession,
    owner: &PersonDraftOwner,
    address: &ActionAddress,
    answer: &mut Pending<Result<CataloguePage, ActionHostError>>,
) -> Step {
    let page = match poll(answer, &waker()) {
        PendingPoll::Waiting => return Step::Waiting,
        PendingPoll::Ready(Ok(page)) => page,
        _ => return Step::Refused,
    };
    if !page.exhausted || page.stale_cursor || page.budget_exhausted {
        return Step::Refused;
    }
    let providers: Vec<_> = page
        .providers
        .iter()
        .filter(|p| p.room == owner.workspace && p.app_id == "so.tilo.chat")
        .collect();
    let [provider] = providers.as_slice() else {
        return Step::Refused;
    };
    Step::Next(Phase::Resolve {
        provider: provider.provider.clone(),
        answer: watch(session.query(CatalogueQuery {
            provider: Some(provider.provider.clone()),
            target: Some(address.clone()),
            max_results: 2,
            ..CatalogueQuery::default()
        })),
    })
}

fn resolve_step(
    session: &DesktopActionSession,
    address: &ActionAddress,
    provider: &str,
    answer: &mut Pending<Result<CataloguePage, ActionHostError>>,
    handoff: bool,
) -> Step {
    let page = match poll(answer, &waker()) {
        PendingPoll::Waiting => return Step::Waiting,
        PendingPoll::Ready(Ok(page)) => page,
        _ => return Step::Refused,
    };
    if !page.exhausted || page.stale_cursor || page.budget_exhausted {
        return Step::Refused;
    }
    let [row] = page.rows.as_slice() else {
        return Step::Refused;
    };
    let Some(descriptor) = &row.action else {
        return Step::Refused;
    };
    if row.provider != provider
        || row.app_id != "so.tilo.chat"
        || row.resource.kind != "chat.person"
        || row.resource.id != address.resource
        || descriptor.address != *address
    {
        return Step::Refused;
    }
    // Prove that this exact certified contact is currently published. Invoke
    // the root effect below: the private broker requires explicit peer/text
    // correlation in the original request, not just a bound row payload.
    Step::Next(Phase::Root {
        provider: provider.into(),
        revision: page.revision.clone(),
        answer: watch(session.query(CatalogueQuery {
            provider: Some(provider.into()),
            needle: if handoff {
                "adopt-private-draft".into()
            } else {
                address.action.clone()
            },
            max_results: 256,
            ..CatalogueQuery::default()
        })),
    })
}

fn root_step(
    session: &DesktopActionSession,
    owner: &PersonDraftOwner,
    text: Option<&str>,
    handoff: Option<uuid::Uuid>,
    selection: (&str, &str),
    answer: &mut Pending<Result<CataloguePage, ActionHostError>>,
) -> Step {
    let (provider, revision) = selection;
    let page = match poll(answer, &waker()) {
        PendingPoll::Waiting => return Step::Waiting,
        PendingPoll::Ready(Ok(page)) => page,
        _ => return Step::Refused,
    };
    if !page.exhausted || page.stale_cursor || page.budget_exhausted {
        return Step::Refused;
    }
    if page.revision != revision {
        return Step::Refused;
    }
    let action = if handoff.is_some() {
        "adopt-private-draft"
    } else if text.is_some() {
        "send-direct-message"
    } else {
        "open-direct-conversation"
    };
    let rows: Vec<_> = page
        .rows
        .iter()
        .filter(|row| {
            row.provider == provider
                && row.app_id == "so.tilo.chat"
                && row.action.as_ref().is_some_and(|a| {
                    a.address.action == action
                        && a.params.iter().any(|p| {
                            p.name
                                == if handoff.is_some() {
                                    "token"
                                } else {
                                    "public_key"
                                }
                        })
                        && (text.is_none() || a.params.iter().any(|p| p.name == "text"))
                })
        })
        .collect();
    let [row] = rows.as_slice() else {
        return Step::Refused;
    };
    let Some(target) = row.target() else {
        return Step::Refused;
    };
    let mut arguments = if let Some(token) = handoff {
        serde_json::Map::from_iter([("token".into(), token.to_string().into())])
    } else {
        serde_json::Map::from_iter([("public_key".into(), owner.person_key.clone().into())])
    };
    if let Some(text) = text {
        arguments.insert("text".into(), text.into());
    }
    let argument = InvokeArgument::Json(arguments);
    let id = InvocationId::new();
    Step::Next(Phase::Invoke {
        id,
        answer: watch(session.invoke(InvokeRequest {
            invocation: id,
            target,
            argument,
            revision: Some(page.revision),
            context: None,
            confirmation: None,
            on_behalf_of: None,
            appearance: None,
            wait_ms: Some(1000),
        })),
    })
}

fn invoke_step(
    session: &DesktopActionSession,
    id: InvocationId,
    answer: &mut Pending<Result<InvocationReceipt, ActionHostError>>,
) -> Step {
    let receipt = match poll(answer, &waker()) {
        PendingPoll::Waiting => return Step::Waiting,
        PendingPoll::Ready(Ok(receipt)) if receipt.invocation == id => receipt,
        _ => return Step::Refused,
    };
    match receipt.status {
        ReceiptStatus::Completed { output } => Step::Complete(output.value),
        ReceiptStatus::Accepted => session.next_change(&id).map_or(Step::Refused, |change| {
            Step::Next(Phase::Observe {
                id,
                change: watch(change),
            })
        }),
        _ => Step::Refused,
    }
}

fn observe_step(
    session: &DesktopActionSession,
    id: InvocationId,
    change: &mut Pending<()>,
) -> Step {
    if matches!(poll(change, &waker()), PendingPoll::Waiting) {
        return Step::Waiting;
    }
    let Some(state) = session.observe(&id) else {
        return Step::Refused;
    };
    match state.update {
        InvocationUpdate::Succeeded { invocation, output } if invocation == id => {
            Step::Complete(output.value)
        }
        InvocationUpdate::Failed { .. } | InvocationUpdate::Cancelled { .. } => Step::Refused,
        _ => session.next_change(&id).map_or(Step::Refused, |change| {
            Step::Next(Phase::Observe {
                id,
                change: watch(change),
            })
        }),
    }
}

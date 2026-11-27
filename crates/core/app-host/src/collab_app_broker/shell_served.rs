//! A provider the person's shell serves on the device's dispatch
//! ([`runtime_collab::wire::shell::ShellCall::Provide`]), as the dispatch reaches it: each call
//! goes to the shell over its link as [`Ask::Action`], carrying the frame an app's connection
//! carries, and what the shell answers is what the dispatch is answered. The registration is the
//! shell's for as long as its link lasts ([`Served::close`]): from then on a call finds no
//! provider, and one still under way ends failed, saying the shell closed. The session's own
//! actions ([`session_actions::PROVIDER`]) are served this way too, as a turn's steps: the layout,
//! its windows and its notices, which the session alone carries out.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use runtime_collab::wire::shell::Typed;
use runtime_collab::wire::{Answer, Ask, Handle};
use tilo_actions::remote::session_actions;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Admitted, Caller, CataloguePage, CatalogueQuery, Completer,
    InvocationReceipt, InvocationState, InvokeRequest, MAX_REMOTE_WAIT, Pending, ReceiptStatus,
    RemoteActionFrame, RemoteActionMessage, UpdateMark,
};
use tilo_actions::{ActionFailure, InvocationId, InvocationUpdate};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tilo_remote_actions::asked::AskedStore;
use tokio::sync::{mpsc, oneshot};

use tilo_workspace_service::workspaces::providers::{Providers, Reach};

/// One ask of the shell, and where its answer goes.
pub(super) type Asking = (Ask, oneshot::Sender<Result<Answer, String>>);

/// The code a call ends failed with once the shell that served its provider closed.
pub const SHELL_CLOSED: &str = "shell_closed";

/// A provider the shell registered on its link, and the registry it stands in.
pub(super) struct Served {
    provider: String,
    link: Arc<LinkProvider>,
    providers: Providers,
}

impl Served {
    /// Register `provider`, offering `catalogue`, in `providers` for the shell whose asks
    /// `asks` carries on `channel`: a provider of the shell's own alone, and one nothing else
    /// serves. `hand_over` hands that shell a file on its link, for the seat's calls that name
    /// one.
    pub(super) fn register(
        providers: &Providers,
        (provider, catalogue): (String, &str),
        channel: Handle,
        asks: mpsc::UnboundedSender<Asking>,
        asked_in: Option<Arc<AskedStore>>,
        hand_over: Option<tilo_workspace_service::workspaces::providers::HandOver>,
        seat: Option<u64>,
    ) -> Result<Self, String> {
        if !runtime_collab::wire::shell::serves(&provider) {
            return Err(format!(
                "the shell serves its own providers, not {provider}"
            ));
        }
        let catalogue = read(&provider, catalogue)?;
        let link = Arc::new_cyclic(|me| LinkProvider {
            me: me.clone(),
            provider: provider.clone(),
            catalogue: RwLock::new(catalogue),
            channel,
            asks,
            runtime: tokio::runtime::Handle::current(),
            calls: std::sync::Mutex::default(),
            asked: std::sync::Mutex::default(),
            answered_first: std::sync::Mutex::default(),
            asked_in,
            seat,
            closed: AtomicBool::new(false),
        });
        // The seat is held apart from every provider a call names: only this device's broker
        // reaches it, for the apps it admitted.
        if provider == crate::seat::PROVIDER {
            providers.serve_seat(
                Arc::clone(&link) as Arc<dyn ActionProvider + Send + Sync>,
                hand_over,
            );
            return Ok(Self {
                provider,
                link,
                providers: providers.clone(),
            });
        }
        // The session's own actions are a turn's steps, under the session's own app; a provider
        // of the person's seat is admitted under the app whose window offers it, as it always
        // was; any other is the shell's. All but the first answer on this device alone.
        let (app, reach) = if provider == session_actions::PROVIDER {
            (session_actions::PROVIDER, Reach::Turns)
        } else if let Some(app) = runtime_collab::wire::shell::seat_app(&provider) {
            (app, Reach::ThisDevice)
        } else {
            (crate::shell::SHELL_APP, Reach::ThisDevice)
        };
        providers.serve(
            &provider,
            app,
            reach,
            Arc::clone(&link) as Arc<dyn ActionProvider + Send + Sync>,
        )?;
        Ok(Self {
            provider,
            link,
            providers: providers.clone(),
        })
    }

    /// The provider's name.
    pub(super) fn provider(&self) -> &str {
        &self.provider
    }

    /// The shell offers what `catalogue` lists from now on.
    pub(super) fn publish(&self, catalogue: &str) -> Result<(), String> {
        *self.link.catalogue.write_recover() = read(&self.provider, catalogue)?;
        Ok(())
    }

    /// The shell let go of the provider, or its link closed: no call reaches it any more, and
    /// one under way ends failed.
    pub(super) fn close(self) {
        let answers = Arc::clone(&self.link) as Arc<dyn ActionProvider + Send + Sync>;
        if self.provider == crate::seat::PROVIDER {
            self.providers.withdraw_seat(&answers);
        } else {
            self.providers.withdraw(&self.provider, &answers);
        }
        self.link.close();
    }
}

/// `catalogue`, what the shell says `provider` offers.
fn read(provider: &str, catalogue: &str) -> Result<CataloguePage, String> {
    serde_json::from_str(catalogue)
        .map_err(|error| format!("the catalogue of {provider} does not read: {error}"))
}

/// What the dispatch calls for a provider the shell serves.
struct LinkProvider {
    me: std::sync::Weak<LinkProvider>,
    provider: String,
    /// What the shell last said the provider offers.
    catalogue: RwLock<CataloguePage>,
    /// The handle the shell answers this provider's calls on.
    channel: Handle,
    asks: mpsc::UnboundedSender<Asking>,
    /// Where a call under way is followed.
    runtime: tokio::runtime::Handle,
    /// The calls taken, as they last stood, and who waits for them to move.
    calls: std::sync::Mutex<HashMap<InvocationId, Followed>>,
    /// The calls the shell answered by asking its person, whose records wait for their answer:
    /// withdrawn once the shell closes, since what asked them is gone.
    asked: std::sync::Mutex<std::collections::HashSet<InvocationId>>,
    /// The calls the shell already answered when the dispatch asked whether they wait for the
    /// person's yes, and did not: what the dispatch's taking of each is answered with.
    answered_first:
        std::sync::Mutex<HashMap<InvocationId, Result<InvocationReceipt, ActionCallError>>>,
    /// Where those records are kept.
    asked_in: Option<Arc<AskedStore>>,
    /// The seat whose shell serves this provider: a program another seat's shell handed over
    /// never reaches it.
    seat: Option<u64>,
    closed: AtomicBool,
}

struct Followed {
    state: InvocationState,
    waiting: Vec<Completer<()>>,
}

impl LinkProvider {
    /// Whether `caller` comes from a program another seat's shell handed over: what this seat's
    /// shell serves is that seat's person's, there, and never the other seat's programs'.
    fn kept_from(&self, caller: &Caller) -> bool {
        caller
            .handing_seat()
            .is_some_and(|seat| Some(seat) != self.seat)
    }

    /// Why a call fails once the shell closed.
    fn gone(&self) -> String {
        format!(
            "the desktop session that served {} has closed",
            self.provider
        )
    }

    /// Ask the shell `message` of `caller`'s, answered with the frame it answers. `typed`, what
    /// the person typed answering a call's record, goes beside the frame, which never carries it.
    fn ask(
        &self,
        caller: &Caller,
        message: RemoteActionMessage,
        typed: Typed,
    ) -> impl Future<Output = Result<RemoteActionMessage, ActionCallError>> + Send + 'static {
        let gone = self.gone();
        let sent = if self.closed.load(Ordering::Acquire) {
            Err(gone.clone())
        } else {
            RemoteActionFrame::new(message)
                .encode()
                .map_err(|error| error.to_string())
                .and_then(|frame| {
                    let (answer, answered) = oneshot::channel();
                    let ask = Ask::Action {
                        channel: self.channel,
                        frame,
                        caller: Box::new(caller.kept()),
                        typed,
                    };
                    self.asks
                        .send((ask, answer))
                        .map(|()| answered)
                        .map_err(|_| gone.clone())
                })
        };
        async move {
            let unavailable = |reason: String| ActionCallError::Unavailable { reason };
            let answered = sent
                .map_err(unavailable)?
                .await
                .map_err(|_| unavailable(gone))?;
            match answered.map_err(unavailable)? {
                Answer::Action(frame) => RemoteActionFrame::decode(&frame)
                    .map(|frame| frame.message)
                    .map_err(|error| unavailable(error.to_string())),
                _ => Err(unavailable(
                    "the shell answered a call with something else".to_owned(),
                )),
            }
        }
    }

    /// Keep `state` as where its call stands, unless the call ended already, and wake whoever
    /// waits for it to move. Answers whether the call goes on.
    fn moved(&self, state: InvocationState) -> bool {
        let mut calls = self.calls.lock_recover();
        let id = state.update.invocation();
        let followed = calls.entry(id).or_insert_with(|| Followed {
            state: state.clone(),
            waiting: Vec::new(),
        });
        if followed.state.is_terminal() {
            return false;
        }
        followed.state = state;
        let waiting: Vec<_> = followed.waiting.drain(..).collect();
        let continues = !followed.state.is_terminal();
        drop(calls);
        for waiting in waiting {
            waiting.complete(());
        }
        continues
    }

    /// No call reaches the shell any more: each under way ends failed, and each record it asked
    /// that waits is withdrawn.
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let asked: Vec<InvocationId> = self.asked.lock_recover().drain().collect();
        if let Some(store) = &self.asked_in {
            for invocation in asked {
                if let Some(at) = store.record(&invocation.0.to_string()) {
                    store.withdraw(&at, &self.gone());
                }
            }
        }
        let calls: Vec<InvocationId> = self.calls.lock_recover().keys().copied().collect();
        for invocation in calls {
            self.moved(InvocationState::new(
                invocation,
                InvocationUpdate::Failed {
                    invocation,
                    error: ActionFailure {
                        code: SHELL_CLOSED.into(),
                        message: self.gone(),
                        details: None,
                        retryable: false,
                    },
                },
            ));
        }
    }
}

/// Follow `caller`'s `invocation` at the shell until it ends there, or the shell closes.
async fn follow(link: Arc<LinkProvider>, caller: Caller, invocation: InvocationId) {
    let mut seen = None;
    loop {
        let asked = link.ask(
            &caller,
            RemoteActionMessage::Observe {
                correlation: 0,
                invocation,
                wait_ms: u32::try_from(MAX_REMOTE_WAIT.as_millis()).ok(),
                seen,
            },
            Typed::default(),
        );
        let Some(answered) = tilo_failure::ignored!(
            "the shell closed: LinkProvider::close already ended the call and told its caller",
            asked.await.ok()
        ) else {
            return;
        };
        let state = match answered {
            RemoteActionMessage::Update {
                state: Some(state), ..
            } => state,
            _ => InvocationState::new(
                invocation,
                InvocationUpdate::Failed {
                    invocation,
                    error: ActionFailure {
                        code: SHELL_CLOSED.into(),
                        message: format!("{} lost the call", link.provider),
                        details: None,
                        retryable: false,
                    },
                },
            ),
        };
        seen = Some(UpdateMark::of(&state.update));
        if !link.moved(state) {
            return;
        }
    }
}

/// What a receipt says of where its call stands.
fn standing(receipt: &InvocationReceipt) -> Option<InvocationUpdate> {
    let invocation = receipt.invocation;
    Some(match &receipt.status {
        ReceiptStatus::Accepted => InvocationUpdate::Accepted { invocation },
        ReceiptStatus::Completed { output } => InvocationUpdate::Succeeded {
            invocation,
            output: output.clone(),
        },
        ReceiptStatus::Failed { error } => InvocationUpdate::Failed {
            invocation,
            error: error.clone(),
        },
        ReceiptStatus::Cancelled { message } => InvocationUpdate::Cancelled {
            invocation,
            message: message.clone(),
        },
        ReceiptStatus::Rejected { .. } => return None,
    })
}

/// How long a follow of a call the shell holds is left before it looks again when nothing moved:
/// the bound a next change promises ([`ActionProvider::next_change`]).
const LOOK_AGAIN: std::time::Duration = std::time::Duration::from_secs(1);

impl ActionProvider for LinkProvider {
    /// What the shell last said it offers, a page at a time: the dispatch lists it at once.
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        if self.kept_from(caller) {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        let offered = self.catalogue.read_recover();
        Pending::ready(Ok(CataloguePage::from_rows(
            offered.revision.clone(),
            offered.rows.clone(),
            &query,
        )))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if self.kept_from(caller) {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        let (done, answer) = tilo_actions::remote::pending();
        let invocation = request.invocation;
        // The person's answer to the call's record comes back as the same call: it asks no more.
        let typed = request.approval().map_or_else(Typed::default, |approval| {
            self.asked.lock_recover().remove(&invocation);
            Typed(approval.typed.clone())
        });
        let asked = self.ask(
            caller,
            RemoteActionMessage::Invoke(InvokeRequest {
                wait_ms: None,
                ..request
            }),
            typed,
        );
        let Some(link) = self.me.upgrade() else {
            done.complete(Err(ActionCallError::Unavailable {
                reason: self.gone(),
            }));
            return answer;
        };
        let caller = caller.clone();
        self.runtime.spawn(async move {
            let receipt = match asked.await {
                Ok(RemoteActionMessage::Receipt(receipt)) => receipt,
                Ok(_) => {
                    done.complete(Err(ActionCallError::Unavailable {
                        reason: "the shell answered a call with something else".into(),
                    }));
                    return;
                }
                Err(error) => {
                    done.complete(Err(error));
                    return;
                }
            };
            if let ReceiptStatus::Rejected { error } = receipt.status {
                if matches!(error, ActionCallError::ConfirmationRequired(_)) {
                    link.asked.lock_recover().insert(invocation);
                }
                done.complete(Err(error));
                return;
            }
            if let Some(update) = standing(&receipt) {
                let goes_on = link.moved(InvocationState::new(receipt.invocation, update));
                if goes_on {
                    tokio::spawn(follow(Arc::clone(&link), caller, receipt.invocation));
                }
            }
            done.complete(Ok(receipt));
        });
        answer
    }

    /// Whether the call waits for the person's yes is the shell's provider's to say, and it says
    /// so only by answering the call: it is asked the call once, here, before the dispatch takes
    /// it, so a refusal for want of the yes is kept as the person's asked record. Any other
    /// answer is what taking the call answers ([`Self::run`]).
    fn asks_first(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        _admitted: &Admitted,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        let invocation = request.invocation;
        let me = self.me.clone();
        Some(
            self.invoke(caller, request.clone())
                .map(move |answered| match answered {
                    Err(waits @ ActionCallError::ConfirmationRequired(_)) => Err(waits),
                    answered => {
                        if let Some(me) = me.upgrade() {
                            me.answered_first
                                .lock_recover()
                                .insert(invocation, answered);
                        }
                        Ok(())
                    }
                }),
        )
    }

    fn run(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        _admitted: Admitted,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let answered = self
            .answered_first
            .lock_recover()
            .remove(&request.invocation);
        match answered {
            Some(answered) => Pending::ready(answered),
            None => self.invoke(caller, request),
        }
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.calls
            .lock_recover()
            .get(id)
            .map(|followed| followed.state.clone())
    }

    fn next_change(&self, _caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        let mut calls = self.calls.lock_recover();
        let followed = calls.get_mut(id)?;
        let (changed, change) = tilo_actions::remote::pending();
        if !followed.state.is_terminal() {
            // A follow that already went waits for nobody.
            followed.waiting.retain(|waiting| !waiting.is_abandoned());
            followed.waiting.push(changed);
            // The shell may hold a call for hours (one its person is asked about): its follower
            // looks again within the bound a next change promises, as it asked to.
            return Some(change.or_after(LOOK_AGAIN));
        }
        drop(calls);
        changed.complete(());
        Some(change)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        let (done, answer) = tilo_actions::remote::pending();
        let asked = self.ask(
            caller,
            RemoteActionMessage::Cancel { invocation: *id },
            Typed::default(),
        );
        self.runtime.spawn(async move {
            done.complete(match asked.await {
                Ok(RemoteActionMessage::CancelResponse { result, .. }) => result,
                Ok(_) => Err(ActionCallError::Unavailable {
                    reason: "the shell answered a cancel with something else".into(),
                }),
                Err(error) => Err(error),
            });
        });
        answer
    }
}

/// Carry the asks the providers of one shell make to it over `ask`, its connection's, and their
/// answers back, until the providers are gone. Runs on the connection's `LocalSet`.
pub(super) async fn carry(
    mut asks: mpsc::UnboundedReceiver<Asking>,
    ask: impl Fn(Ask) -> Option<oneshot::Receiver<Result<Answer, String>>>,
) {
    while let Some((asked, reply)) = asks.recv().await {
        let Some(answered) = ask(asked) else {
            let _ = reply.send(Err("the shell's link has closed".to_owned()));
            continue;
        };
        tokio::task::spawn_local(async move {
            let _ = reply.send(
                answered
                    .await
                    .unwrap_or_else(|_| Err("the shell's link has closed".to_owned())),
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use tilo_actions::remote::{
        ActionCallError, ActionProvider as _, Caller, CataloguePage, CatalogueQuery, InvokeRequest,
        PendingPoll, RemoteActionTarget, settings_resources,
    };
    use tilo_workspace_service::workspaces::providers::Providers;

    /// What a seat's shell serves (Settings' sections here) reaches a program that seat's shell
    /// handed over, and never one another seat's shell handed over: the program comes from the
    /// other seat, whose person's settings are not these.
    #[tokio::test]
    async fn a_program_another_seat_handed_over_never_reaches_this_seats_providers() {
        let providers = Providers::default();
        let (asks, _asked) = tokio::sync::mpsc::unbounded_channel();
        let catalogue = serde_json::to_string(&CataloguePage::from_rows(
            String::new(),
            Vec::new(),
            &CatalogueQuery::default(),
        ))
        .unwrap();
        let _served = super::Served::register(
            &providers,
            (settings_resources::PROVIDER.to_owned(), &catalogue),
            runtime_collab::wire::Handle(1),
            asks,
            None,
            None,
            Some(1),
        )
        .expect("the seat's shell serves its settings");
        let settings = providers
            .get(settings_resources::PROVIDER)
            .expect("registered")
            .answers;
        let program = |seat| {
            Caller::local_person("ab".repeat(32))
                .in_workspace("w")
                .handed_by_seat(seat, "/usr/bin/tilo")
        };
        let listed = |caller: &Caller| match settings
            .query(caller, CatalogueQuery::default())
            .poll_ready()
        {
            PendingPoll::Ready(listed) => listed.map(|_| ()),
            _ => panic!("a listing is answered at once"),
        };
        assert_eq!(
            listed(&program(1)),
            Ok(()),
            "its own seat's program lists it"
        );
        assert_eq!(
            listed(&program(2)),
            Err(ActionCallError::NotPermitted),
            "another seat's program is kept out"
        );
        let request: InvokeRequest = serde_json::from_value(serde_json::json!({
            "invocation": tilo_actions::InvocationId::new(),
            "target": RemoteActionTarget {
                provider: settings_resources::PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new("settings.appearance", "set"),
            },
        }))
        .unwrap();
        match settings.invoke(&program(2), request).poll_ready() {
            PendingPoll::Ready(called) => assert_eq!(
                called.map(|_| ()),
                Err(ActionCallError::NotPermitted),
                "another seat's program changes nothing here"
            ),
            _ => panic!("another seat's program is refused at once"),
        }
    }
}

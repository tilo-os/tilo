//! The device's one dispatch as the person's shell reaches it over its link
//! ([`crate::wire::shell`]), both ways: [`ShellDispatch`] carries the shell's own calls to it as
//! the person, and [`ShellClient::provide`] serves a provider of the shell's on it. Each carries
//! the `tilo_actions::remote::RemoteActionFrame`s an app's connection carries.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use runtime_portal::NotAvailable;
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Confirmation,
    InvocationReceipt, InvocationState, InvokeRequest, MAX_REMOTE_WAIT, Pending, ReceiptStatus,
    RemoteActionFrame, RemoteActionMessage, ResourceReadRequest, ResourceReads, UpdateMark,
};

use crate::ShellClient;
use crate::wire::shell::{ShellCall, ShellReply};
use crate::wire::{Answer, Ask, Handle};

/// How long a look at where a call stands waits for the daemon's answer.
const LOOK: Duration = Duration::from_secs(10);

/// How long a wait for a call to move is held at the daemon before it answers as it stands.
fn held() -> Option<u32> {
    u32::try_from(MAX_REMOTE_WAIT.as_millis()).ok()
}

/// The device's one dispatch, as the shell's person: every call goes on the shell's link
/// ([`ShellCall::Action`]), in the workspace its caller names. Cheap to clone.
#[derive(Clone)]
pub struct ShellDispatch(Arc<Inner>);

struct Inner {
    shell: ShellClient,
    runtime: tokio::runtime::Handle,
    /// Where each call the daemon told of last stood, from a wait for it to move: what the look
    /// that follows the wake reads, so a change costs one round trip.
    fresh: Mutex<HashMap<InvocationId, (InvocationState, Option<String>)>>,
    next_correlation: AtomicU64,
}

impl ShellDispatch {
    pub(crate) fn new(shell: ShellClient) -> Self {
        Self(Arc::new(Inner {
            shell,
            runtime: tokio::runtime::Handle::current(),
            fresh: Mutex::default(),
            next_correlation: AtomicU64::new(1),
        }))
    }

    /// `message` asked of the daemon's dispatch in `workspace`, answered with the frame that
    /// answers it.
    async fn ask(
        shell: &ShellClient,
        workspace: Option<String>,
        message: RemoteActionMessage,
    ) -> Result<RemoteActionMessage, ActionCallError> {
        Self::ask_as(shell, workspace, None, message).await
    }

    /// [`Self::ask`], as `caller` ([`named`]) where it names one.
    async fn ask_as(
        shell: &ShellClient,
        workspace: Option<String>,
        caller: Option<tilo_actions::remote::KeptCaller>,
        message: RemoteActionMessage,
    ) -> Result<RemoteActionMessage, ActionCallError> {
        let unavailable = |reason: String| ActionCallError::Unavailable { reason };
        let frame = RemoteActionFrame::new(message)
            .encode()
            .map_err(|error| unavailable(error.to_string()))?;
        match shell
            .call(ShellCall::Action {
                workspace,
                frame,
                caller,
            })
            .await
            .map_err(|error| unavailable(error.to_string()))?
        {
            ShellReply::Action(frame) => RemoteActionFrame::decode(&frame)
                .map(|frame| frame.message)
                .map_err(|error| unavailable(error.to_string())),
            other => Err(unavailable(format!(
                "the daemon answered an action with {other:?}"
            ))),
        }
    }

    /// [`Self::ask`] on the dispatch's runtime, answered with what `answer` reads of its reply.
    fn asking<T: Send + 'static>(
        &self,
        caller: &Caller,
        message: RemoteActionMessage,
        answer: impl FnOnce(RemoteActionMessage) -> Result<T, ActionCallError> + Send + 'static,
    ) -> Pending<Result<T, ActionCallError>> {
        let (done, result) = tilo_actions::remote::pending();
        let shell = self.0.shell.clone();
        let workspace = caller.workspace().map(str::to_owned);
        let named = named(caller);
        self.0.runtime.spawn(async move {
            done.complete(
                Self::ask_as(&shell, workspace, named, message)
                    .await
                    .and_then(answer),
            );
        });
        result
    }

    /// Invoke `request` in `workspace` and follow it until it ends: the value it answered, or
    /// why it failed, as the person reads it.
    ///
    /// # Errors
    /// The action was refused, failed or was cancelled, or the daemon could not be asked.
    pub async fn settled(
        &self,
        workspace: Option<String>,
        mut request: InvokeRequest,
    ) -> Result<serde_json::Value, String> {
        let invocation = request.invocation;
        request.wait_ms = held();
        let shell = &self.0.shell;
        let receipt = match Self::ask(
            shell,
            workspace.clone(),
            RemoteActionMessage::Invoke(request),
        )
        .await
        .map_err(|error| error.to_string())?
        {
            RemoteActionMessage::Receipt(receipt) => receipt,
            other => return Err(format!("the daemon answered an action with {other:?}")),
        };
        let mut update = match receipt.status {
            ReceiptStatus::Accepted => tilo_actions::InvocationUpdate::Accepted { invocation },
            status => return settled(status),
        };
        loop {
            if !matches!(
                update,
                tilo_actions::InvocationUpdate::Accepted { .. }
                    | tilo_actions::InvocationUpdate::Progress { .. }
                    | tilo_actions::InvocationUpdate::Item { .. }
            ) {
                return finished(update);
            }
            let observe = RemoteActionMessage::Observe {
                correlation: 0,
                invocation,
                wait_ms: held(),
                seen: Some(UpdateMark::of(&update)),
            };
            update = match Self::ask(shell, workspace.clone(), observe)
                .await
                .map_err(|error| error.to_string())?
            {
                RemoteActionMessage::Update {
                    state: Some(state), ..
                } => state.update,
                RemoteActionMessage::Update { state: None, .. } => {
                    return Err("the daemon lost the action".to_owned());
                }
                other => return Err(format!("the daemon answered a look with {other:?}")),
            };
        }
    }
}

/// What a receipt that ended at once comes to.
fn settled(status: ReceiptStatus) -> Result<serde_json::Value, String> {
    match status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Rejected { error } => Err(error.to_string()),
        ReceiptStatus::Failed { error } => Err(error.message),
        ReceiptStatus::Cancelled { message } => {
            Err(message.unwrap_or_else(|| "the action was cancelled".to_owned()))
        }
        ReceiptStatus::Accepted => Err("the action did not finish".to_owned()),
    }
}

/// What a call that ended comes to.
fn finished(update: tilo_actions::InvocationUpdate) -> Result<serde_json::Value, String> {
    use tilo_actions::InvocationUpdate;
    match update {
        InvocationUpdate::Succeeded { output, .. } => Ok(output.value.unwrap_or_default()),
        InvocationUpdate::Failed { error, .. } => Err(error.message),
        InvocationUpdate::Cancelled { message, .. } => {
            Err(message.unwrap_or_else(|| "the action was cancelled".to_owned()))
        }
        InvocationUpdate::Accepted { .. }
        | InvocationUpdate::Progress { .. }
        | InvocationUpdate::Item { .. } => Err("the action did not finish".to_owned()),
    }
}

fn unexpected() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: "the daemon answered with something else".into(),
    }
}

/// Who the daemon decides a call as: `None` for the person the shell's link was made for (and for
/// the shell's own system work, which the link carries as that person), else the caller itself,
/// an app or an agent acting for them.
fn named(caller: &Caller) -> Option<tilo_actions::remote::KeptCaller> {
    let acting = caller.acting();
    let person = acting.is_person()
        && acting.app().is_none()
        && !caller.is_session_delegated()
        && !caller.is_foreign_device();
    (!person && !caller.is_system()).then(|| caller.kept())
}

/// A read of a record outside its app, asked of the daemon, which holds the workspaces and decides
/// it there as the caller reading: the person, or an app or an agent acting for them.
impl ResourceReads for ShellDispatch {
    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> Pending<RemoteActionMessage> {
        let (done, result) = tilo_actions::remote::pending();
        let named = named(caller);
        let shell = self.0.shell.clone();
        let workspace = Some(read.workspace.clone());
        let message = RemoteActionMessage::ResourceRead {
            correlation,
            on_behalf_of: None,
            read,
        };
        self.0.runtime.spawn(async move {
            done.complete(
                Self::ask_as(&shell, workspace, named, message)
                    .await
                    .unwrap_or_else(|error| RemoteActionMessage::ResourceReading {
                        correlation,
                        result: Err(error),
                    }),
            );
        });
        result
    }
}

impl ActionProvider for ShellDispatch {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        self.asking(
            caller,
            RemoteActionMessage::CatalogueRequest { correlation, query },
            |answer| match answer {
                RemoteActionMessage::CatalogueResponse { result, .. } => result,
                _ => Err(unexpected()),
            },
        )
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.asking(
            caller,
            RemoteActionMessage::Invoke(request),
            |answer| match answer {
                RemoteActionMessage::Receipt(receipt) => match receipt.status {
                    ReceiptStatus::Rejected { error } => Err(error),
                    _ => Ok(receipt),
                },
                _ => Err(unexpected()),
            },
        )
    }

    /// What the wait that just woke its caller brought back, else the daemon's answer now.
    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let workspace = caller.workspace().map(str::to_owned);
        let fresh = self
            .0
            .fresh
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id)
            .filter(|(_, of)| *of == workspace);
        if let Some((state, _)) = fresh {
            return Some(state);
        }
        let look = self.asking(
            caller,
            RemoteActionMessage::Observe {
                correlation: 0,
                invocation: *id,
                wait_ms: None,
                seen: None,
            },
            |answer| match answer {
                RemoteActionMessage::Update { state, .. } => Ok(state),
                _ => Err(unexpected()),
            },
        );
        match look.wait_timeout(LOOK) {
            Ok(Some(Ok(state))) => state,
            _ => None,
        }
    }

    /// A wait the daemon holds until the call moves past where it was last seen.
    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        let seen = self
            .0
            .fresh
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .map(|(state, _)| UpdateMark::of(&state.update));
        let inner = Arc::clone(&self.0);
        let (invocation, workspace) = (*id, caller.workspace().map(str::to_owned));
        let (changed, change) = tilo_actions::remote::pending();
        self.0.runtime.spawn(async move {
            let asked = Self::ask(
                &inner.shell,
                workspace.clone(),
                RemoteActionMessage::Observe {
                    correlation: 0,
                    invocation,
                    wait_ms: held(),
                    seen,
                },
            )
            .await;
            if let Ok(RemoteActionMessage::Update {
                state: Some(state), ..
            }) = asked
            {
                inner
                    .fresh
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(invocation, (state, workspace));
            }
            changed.complete(());
        });
        Some(change)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        self.asking(
            caller,
            RemoteActionMessage::Cancel { invocation: *id },
            |answer| match answer {
                RemoteActionMessage::CancelResponse { result, .. } => result,
                _ => Err(unexpected()),
            },
        )
    }
}

/// A provider the shell serves on the device's dispatch ([`ShellClient::provide`]). Dropping it
/// leaves it served until the link closes; [`Self::withdraw`] ends it sooner.
pub struct Provided {
    shell: ShellClient,
    provider: String,
    handle: Handle,
}

impl Provided {
    /// The handle its calls arrive on.
    #[must_use]
    pub const fn handle(&self) -> Handle {
        self.handle
    }

    /// Offer what `catalogue` lists from now on: what the dispatch lists, and admits calls of.
    ///
    /// # Errors
    /// The link closed, or the daemon refused the catalogue.
    pub async fn publish(&self, catalogue: &CataloguePage) -> Result<(), NotAvailable> {
        let handle = self.shell.offer(&self.provider, catalogue).await?;
        if handle == self.handle {
            Ok(())
        } else {
            Err(NotAvailable::Refused(format!(
                "the daemon serves {} on another handle",
                self.provider
            )))
        }
    }

    /// Serve it no more: the dispatch finds no such provider from now on.
    pub fn withdraw(self) {
        self.shell.client.client.close(self.handle);
    }
}

impl ShellClient {
    /// The device's one dispatch, as this shell's person, on this link.
    #[must_use]
    pub fn dispatch(&self) -> ShellDispatch {
        ShellDispatch::new(self.clone())
    }

    /// Serve `provider`, a provider of the shell's own ([`crate::wire::shell::serves`]), on the
    /// device's dispatch with `answers`, offering what `catalogue` lists: each call the dispatch
    /// admits reaches `answers` as the caller it admitted it for, until the link closes or the
    /// shell withdraws it.
    ///
    /// # Errors
    /// The daemon refused the provider: not the shell's own, or served by another already.
    pub async fn provide(
        &self,
        provider: &str,
        catalogue: &CataloguePage,
        answers: Arc<dyn ActionProvider + Send + Sync>,
    ) -> Result<Provided, NotAvailable> {
        let handle = self.offer(provider, catalogue).await?;
        self.client.client.answer(
            handle,
            Arc::new(move |ask| {
                let answers = Arc::clone(&answers);
                Box::pin(async move { serve(&answers, ask).await })
            }),
        );
        Ok(Provided {
            shell: self.clone(),
            provider: provider.to_owned(),
            handle,
        })
    }

    /// Tell the daemon `provider` offers what `catalogue` lists, answered with the handle its
    /// calls arrive on.
    async fn offer(
        &self,
        provider: &str,
        catalogue: &CataloguePage,
    ) -> Result<Handle, NotAvailable> {
        let catalogue = serde_json::to_string(catalogue)
            .map_err(|error| NotAvailable::Refused(error.to_string()))?;
        match self
            .call(ShellCall::Provide {
                provider: provider.to_owned(),
                catalogue,
            })
            .await?
        {
            ShellReply::Providing(handle) => Ok(handle),
            other => Err(NotAvailable::Refused(format!(
                "the daemon answered a provider with {other:?}"
            ))),
        }
    }
}

/// `ask`, a call of a provider the shell serves, answered by `answers` as the caller it names.
async fn serve(
    answers: &Arc<dyn ActionProvider + Send + Sync>,
    ask: Ask,
) -> Result<Answer, String> {
    let Ask::Action {
        frame,
        caller,
        typed,
        ..
    } = ask
    else {
        return Err("a provider the shell serves answers calls alone".to_owned());
    };
    // Who the call is the daemon's word, on the link it made for this shell alone.
    let caller = caller.carried();
    let mut message = RemoteActionFrame::decode(&frame)
        .map_err(|error| error.to_string())?
        .message;
    // What the person typed answering the call's record travels beside the frame, never in it,
    // and goes back into the approve it answers: to the one call, on this device alone.
    if !typed.0.is_empty() {
        let RemoteActionMessage::Invoke(InvokeRequest {
            confirmation: Some(Confirmation::Asked(approval)),
            ..
        }) = &mut message
        else {
            return Err("typed answers go with the approve of a call's record alone".to_owned());
        };
        approval.typed = typed.0;
    }
    let answer = tilo_actions::remote::serve(answers, &caller, message)
        .ok_or("a provider answers requests, and that frame is none")?
        .await
        .ok_or("the provider dropped the call")?;
    RemoteActionFrame::new(answer)
        .encode()
        .map(Answer::Action)
        .map_err(|error| error.to_string())
}

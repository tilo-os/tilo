//! The device's one dispatch as the person's shell reaches it over its link
//! ([`crate::wire::shell`]), both ways: [`ShellDispatch`] carries the shell's own calls to it as
//! the person, and [`ShellClient::provide`] serves a provider of the shell's on it. Each carries
//! the `tilo_actions::remote::RemoteActionFrame`s an app's connection carries.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use runtime_portal::NotAvailable;
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Confirmation,
    InvocationReceipt, InvocationState, InvokeRequest, MAX_REMOTE_WAIT, Pending, ReceiptStatus,
    RemoteActionFrame, RemoteActionMessage, ResourceReadRequest, ResourceReads, UpdateMark,
};

use super::Served;
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
    /// The one thread that drives [`Self::runtime`] when it has only one: the thread every answer
    /// of the daemon arrives on, so a look that waits there for one waits for itself.
    driver: OnceLock<std::thread::ThreadId>,
    /// Where each call the daemon told of last stood, from its receipt or from a wait for it to
    /// move: what the look that follows reads, so a change costs one round trip, and a look made
    /// on the link's own thread (a wait's look after its wake, which runs where the wake arrived)
    /// is answered without the daemon.
    fresh: Mutex<Fresh>,
    next_correlation: AtomicU64,
}

/// What the daemon last said of each call a look has not read yet: where it stood, or `None` when
/// the daemon has no state of it for its caller, with the workspace it was asked in.
#[derive(Default)]
struct Fresh {
    told: HashMap<InvocationId, (Option<InvocationState>, Option<String>, u64)>,
    next: u64,
}

/// The most calls whose last word is kept unread: one a caller never looks at again goes once
/// this many newer ones were told.
const MAX_FRESH: usize = 4096;

impl Fresh {
    fn tell(
        &mut self,
        invocation: InvocationId,
        state: Option<InvocationState>,
        workspace: Option<String>,
    ) {
        if self.told.len() >= MAX_FRESH && !self.told.contains_key(&invocation) {
            let oldest = self
                .told
                .iter()
                .min_by_key(|(_, (_, _, order))| *order)
                .map(|(invocation, _)| *invocation);
            if let Some(oldest) = oldest {
                self.told.remove(&oldest);
            }
        }
        self.next = self.next.wrapping_add(1);
        self.told.insert(invocation, (state, workspace, self.next));
    }
}

impl ShellDispatch {
    pub(crate) fn new(shell: ShellClient) -> Self {
        let runtime = shell.runtime();
        let inner = Arc::new(Inner {
            runtime: runtime.clone(),
            shell,
            driver: OnceLock::new(),
            fresh: Mutex::default(),
            next_correlation: AtomicU64::new(1),
        });
        if runtime.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread {
            let noted = Arc::clone(&inner);
            runtime.spawn(async move {
                let _ = noted.driver.set(std::thread::current().id());
            });
        }
        Self(inner)
    }

    /// Whether this thread drives the link's runtime: a wait here for the daemon's answer would
    /// stop the very thread that carries it, and every other ask of the desktop's behind it.
    fn on_the_link(&self) -> bool {
        self.0.driver.get() == Some(&std::thread::current().id())
    }

    /// Keep what the daemon said of `invocation`, asked in `workspace`, for the next look at it.
    fn tell(
        &self,
        invocation: InvocationId,
        state: Option<InvocationState>,
        workspace: Option<String>,
    ) {
        self.0
            .fresh
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .tell(invocation, state, workspace);
    }

    /// `message` asked of the daemon's dispatch in `workspace`, answered with the frame that
    /// answers it.
    async fn ask(
        shell: &ShellClient,
        workspace: Option<String>,
        message: RemoteActionMessage,
    ) -> Result<RemoteActionMessage, ActionCallError> {
        Self::ask_as(shell, workspace, None, message, (None, None)).await
    }

    /// [`Self::ask`], as `caller` ([`named`]) where it names one, with the shell's window when
    /// the person pressed the call there (`pressed`), and naming `press`, the compositor's serial
    /// of the person's press on the shell's own surfaces that made it, when one did.
    async fn ask_as(
        shell: &ShellClient,
        workspace: Option<String>,
        caller: Option<Box<tilo_actions::remote::KeptCaller>>,
        message: RemoteActionMessage,
        (pressed, press): (Option<crate::wire::shell::ShellPress>, Option<u32>),
    ) -> Result<RemoteActionMessage, ActionCallError> {
        let unavailable = |reason: String| ActionCallError::Unavailable { reason };
        let frame = RemoteActionFrame::new(message)
            .pressed(press)
            .encode()
            .map_err(|error| unavailable(error.to_string()))?;
        match shell
            .call(ShellCall::Action {
                workspace,
                frame,
                caller,
                pressed,
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
        // The person's own call, made now by their press on the shell's own surfaces, names it.
        let press = (named.is_none() && matches!(message, RemoteActionMessage::Invoke(_)))
            .then(tilo_actions::remote::press_points::handled)
            .flatten();
        self.0.runtime.spawn(async move {
            done.complete(
                Self::ask_as(&shell, workspace, named, message, (None, press))
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
        request: InvokeRequest,
    ) -> Result<serde_json::Value, String> {
        Self::settled_output(&self.0.shell, workspace, request, None)
            .await
            .map(|output| output.value.unwrap_or_default())
    }

    /// [`Self::settled`] on the dispatch's own runtime, for a caller that is not on one: what the
    /// action answered whole, told once it ended.
    #[must_use]
    pub fn follow(
        &self,
        workspace: Option<String>,
        request: InvokeRequest,
    ) -> Pending<Result<tilo_actions::ActionOutput, String>> {
        let (done, result) = tilo_actions::remote::pending();
        let shell = self.0.shell.clone();
        // The person's own call, made now by their press on the shell's own surfaces, names it.
        let press = tilo_actions::remote::press_points::handled();
        self.0.runtime.spawn(async move {
            done.complete(Self::settled_output(&shell, workspace, request, press).await);
        });
        result
    }

    /// Every action the apps publish on the device's dispatch that the shell's person is offered in
    /// each of `workspaces`, as rows of each app's listing, read on the dispatch's own runtime: what
    /// the shell offers in its search and on an app's links, whether the app is open or closed. Each
    /// workspace is asked on its own, as the dispatch admits the person there; one that refuses
    /// (one this device is not admitted to yet) lists nothing, and the others still list theirs:
    /// the answer fails only when every workspace named refused, saying why the first did.
    #[must_use]
    pub fn app_actions(
        &self,
        workspaces: Vec<String>,
    ) -> Pending<Result<Vec<tilo_actions::remote::CatalogueRow>, String>> {
        let (done, result) = tilo_actions::remote::pending();
        let shell = self.0.shell.clone();
        self.0.runtime.spawn(async move {
            let mut rows = Vec::new();
            let mut refused = None;
            let mut answered = workspaces.is_empty();
            for workspace in workspaces {
                match Self::app_rows(&shell, &workspace).await {
                    Ok(listed) => {
                        answered = true;
                        rows.extend(listed);
                    }
                    Err(error) => {
                        refused.get_or_insert_with(|| format!("{workspace}: {error}"));
                    }
                }
            }
            done.complete(match refused {
                Some(error) if !answered => Err(error),
                _ => Ok(rows),
            });
        });
        result
    }

    async fn app_rows(
        shell: &ShellClient,
        workspace: &str,
    ) -> Result<Vec<tilo_actions::remote::CatalogueRow>, ActionCallError> {
        let publishers = CatalogueQuery {
            max_results: 0,
            ..CatalogueQuery::default()
        };
        let providers = Self::page(shell, workspace, publishers).await?.providers;
        let mut rows = Vec::new();
        for provider in providers {
            if tilo_actions::remote::application_provider(&provider.provider).is_none() {
                continue;
            }
            let mut query = CatalogueQuery {
                provider: Some(provider.provider.clone()),
                max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                ..CatalogueQuery::default()
            };
            loop {
                let page = Self::page(shell, workspace, query.clone()).await?;
                rows.extend(
                    tilo_actions::remote::expand(page.rows)
                        .into_iter()
                        .filter(|row| row.action.is_some()),
                );
                if page.exhausted || page.next_cursor.is_empty() {
                    break;
                }
                query.revision = Some(page.revision);
                query.cursor = page.next_cursor;
            }
        }
        Ok(rows)
    }

    /// One page of the person's catalogue in `workspace`, asked of the daemon.
    async fn page(
        shell: &ShellClient,
        workspace: &str,
        query: CatalogueQuery,
    ) -> Result<CataloguePage, ActionCallError> {
        match Self::ask(
            shell,
            Some(workspace.to_owned()),
            RemoteActionMessage::CatalogueRequest {
                correlation: 0,
                query,
            },
        )
        .await?
        {
            RemoteActionMessage::CatalogueResponse { result, .. } => result,
            _ => Err(unexpected()),
        }
    }

    async fn settled_output(
        shell: &ShellClient,
        workspace: Option<String>,
        mut request: InvokeRequest,
        press: Option<u32>,
    ) -> Result<tilo_actions::ActionOutput, String> {
        let invocation = request.invocation;
        request.wait_ms = held();
        let receipt = match Self::ask_as(
            shell,
            workspace.clone(),
            None,
            RemoteActionMessage::Invoke(request),
            (None, press),
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
fn settled(status: ReceiptStatus) -> Result<tilo_actions::ActionOutput, String> {
    match status {
        ReceiptStatus::Completed { output } => Ok(output),
        ReceiptStatus::Rejected { error } => Err(error.to_string()),
        ReceiptStatus::Failed { error } => Err(error.message),
        ReceiptStatus::Cancelled { message } => {
            Err(message.unwrap_or_else(|| "the action was cancelled".to_owned()))
        }
        ReceiptStatus::Accepted => Err("the action did not finish".to_owned()),
    }
}

/// What a call that ended comes to.
fn finished(update: tilo_actions::InvocationUpdate) -> Result<tilo_actions::ActionOutput, String> {
    use tilo_actions::InvocationUpdate;
    match update {
        InvocationUpdate::Succeeded { output, .. } => Ok(output),
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
fn named(caller: &Caller) -> Option<Box<tilo_actions::remote::KeptCaller>> {
    let acting = caller.acting();
    let person = acting.is_person()
        && acting.app().is_none()
        && !caller.is_session_delegated()
        && !caller.is_foreign_device();
    (!person && !caller.is_system()).then(|| Box::new(caller.kept()))
}

/// A read or a change of a record outside its app, asked of the daemon, which holds the
/// workspaces and decides it there as the caller asking: the person, or an app or an agent acting
/// for them.
impl ResourceReads for ShellDispatch {
    /// Asked of the daemon as a read that answers only its decision, and followed as every call
    /// on the link is: the record's copy never crosses the link for it.
    fn may_read<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a tilo_actions::remote::ResourceAddress,
    ) -> tilo_actions::remote::ReadsAnswer<'a, ()> {
        let read = ResourceReadRequest {
            workspace: address.workspace.clone(),
            app: address.app.clone(),
            record: address.record.clone(),
            joined: false,
            after: None,
            wait_ms: None,
            answer: tilo_actions::remote::ReadAnswer::Decision,
        };
        let this = Arc::new(self.clone());
        Box::pin(async move {
            match this.answer_read(caller, 0, read).await {
                RemoteActionMessage::ResourceReading { result, .. } => result.map(|_| ()),
                _ => Err(unexpected()),
            }
        })
    }

    /// Asked of the daemon as a change's decision alone, as [`Self::may_read`] asks a read's: the
    /// record's copy never crosses the link for it.
    fn may_write<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a tilo_actions::remote::ResourceAddress,
    ) -> tilo_actions::remote::ReadsAnswer<'a, ()> {
        let read = ResourceReadRequest {
            workspace: address.workspace.clone(),
            app: address.app.clone(),
            record: address.record.clone(),
            joined: false,
            after: None,
            wait_ms: None,
            answer: tilo_actions::remote::ReadAnswer::WriteDecision,
        };
        let this = Arc::new(self.clone());
        Box::pin(async move {
            match this.answer_read(caller, 0, read).await {
                RemoteActionMessage::ResourceReading { result, .. } => match result {
                    // A decision answers `null`; a reader that read the record instead cannot
                    // say whether it may be changed, so its copy is no answer.
                    Ok(answer) if answer.trim() == "null" => Ok(()),
                    Ok(_) => Err(unexpected()),
                    Err(error) => Err(error),
                },
                _ => Err(unexpected()),
            }
        })
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> tilo_actions::remote::ReadingAnswer {
        let named = named(caller);
        let shell = self.0.shell.clone();
        let workspace = Some(read.workspace.clone());
        let message = RemoteActionMessage::ResourceRead {
            correlation,
            on_behalf_of: None,
            read,
        };
        Box::pin(async move {
            Self::ask_as(&shell, workspace, named, message, (None, None))
                .await
                .unwrap_or_else(|error| RemoteActionMessage::ResourceReading {
                    correlation,
                    result: Err(error),
                })
        })
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
        let this = self.clone();
        let workspace = caller.workspace().map(str::to_owned);
        self.asking(
            caller,
            RemoteActionMessage::Invoke(request),
            move |answer| match answer {
                RemoteActionMessage::Receipt(receipt) => match receipt.status {
                    ReceiptStatus::Rejected { error } => Err(error),
                    // A call taken up and still running is where its first look finds it: a wait
                    // for it to settle looks as soon as its receipt arrives, on the link's thread.
                    ReceiptStatus::Accepted => {
                        let invocation = receipt.invocation;
                        this.tell(
                            invocation,
                            Some(InvocationState::new(
                                invocation,
                                tilo_actions::InvocationUpdate::Accepted { invocation },
                            )),
                            workspace,
                        );
                        Ok(receipt)
                    }
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
            .told
            .remove(id)
            .filter(|(_, of, _)| *of == workspace);
        if let Some((state, _, _)) = fresh {
            return state;
        }
        // The daemon's answer arrives on this very thread: waiting for it here would hold the
        // link, and every ask of the desktop's behind it, until the look gave up. Nothing told
        // of this call is known here, which is what the look says.
        if self.on_the_link() {
            tracing::error!(
                invocation = %id,
                from = %std::backtrace::Backtrace::force_capture(),
                "a look at a call was made on the thread that carries the daemon's link, with \
                 nothing told of it yet"
            );
            return None;
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
            .told
            .get(id)
            .and_then(|(state, _, _)| state.as_ref())
            .map(|state| UpdateMark::of(&state.update));
        let this = self.clone();
        let (invocation, workspace) = (*id, caller.workspace().map(str::to_owned));
        let (changed, change) = tilo_actions::remote::pending();
        self.0.runtime.spawn(async move {
            let asked = Self::ask(
                &this.0.shell,
                workspace.clone(),
                RemoteActionMessage::Observe {
                    correlation: 0,
                    invocation,
                    wait_ms: held(),
                    seen,
                },
            )
            .await;
            // Whatever the daemon answered is what the look this wakes reads: that look runs on
            // this thread, where it cannot wait for the daemon.
            let state = match asked {
                Ok(RemoteActionMessage::Update { state, .. }) => state,
                Ok(_) => {
                    tracing::warn!(%invocation, "the daemon answered a wait for a call with something else");
                    None
                }
                Err(error) => {
                    tracing::warn!(%invocation, %error, "the daemon did not answer a wait for a call");
                    None
                }
            };
            this.tell(invocation, state, workspace);
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
}

impl Provided {
    /// Offer what `catalogue` lists from now on: what the dispatch lists, and admits calls of.
    /// The daemon is handed it again on a new link, so a link that ended is no error here.
    ///
    /// # Errors
    /// The daemon refused the catalogue.
    pub async fn publish(&self, catalogue: &CataloguePage) -> Result<(), NotAvailable> {
        self.shell.publish_provider(&self.provider, catalogue).await
    }

    /// Serve it no more: the dispatch finds no such provider from now on.
    pub fn withdraw(self) {
        let Ok(mut served) = self.shell.served.try_lock() else {
            return;
        };
        if let Some(entry) = served.remove(&self.provider)
            && let Some(handle) = entry.handle
        {
            self.shell.current().client.close(handle);
        }
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
    /// shell withdraws it. It is offered again on each new link.
    ///
    /// # Errors
    /// The daemon refused the provider: not the shell's own, or served by another already.
    pub async fn provide(
        &self,
        provider: &str,
        catalogue: &CataloguePage,
        answers: Arc<dyn ActionProvider + Send + Sync>,
    ) -> Result<Provided, NotAvailable> {
        // A provider this shell served already is its own: it is not taken over in its place,
        // and the handle it has offers what it lists from now on.
        if self.served.lock().await.contains_key(provider) {
            return Err(NotAvailable::Refused(format!(
                "{provider} is served on this shell already"
            )));
        }
        let handle = match self.offer(provider, catalogue).await {
            Ok(handle) => handle,
            Err(refused) => return Err(refused),
        };
        answer_provider(&self.current(), handle, Arc::clone(&answers));
        self.served.lock().await.insert(
            provider.to_owned(),
            Served {
                catalogue: catalogue.clone(),
                answers,
                handle: Some(handle),
            },
        );
        Ok(Provided {
            shell: self.clone(),
            provider: provider.to_owned(),
        })
    }

    /// Offer what `catalogue` lists for `provider` from now on, on the link that stands now: what
    /// the daemon lists, and admits calls of. A link that ended keeps it for the next one.
    async fn publish_provider(
        &self,
        provider: &str,
        catalogue: &CataloguePage,
    ) -> Result<(), NotAvailable> {
        let answers = {
            let mut served = self.served.lock().await;
            let Some(entry) = served.get_mut(provider) else {
                return Err(NotAvailable::Refused(format!(
                    "{provider} is served on this shell no more"
                )));
            };
            entry.catalogue = catalogue.clone();
            let answers = Arc::clone(&entry.answers);
            drop(served);
            answers
        };
        // A link that ended offers it again when it comes back, with what this last said.
        if !matches!(self.link_status(), super::ShellLink::Linked) {
            return Ok(());
        }
        let handle = match offer_on(&self.current(), provider, catalogue).await {
            Ok(handle) => handle,
            Err(NotAvailable::ConnectionFailed(_)) => return Ok(()),
            Err(refused) => return Err(refused),
        };
        let renewed = {
            let mut served = self.served.lock().await;
            let renewed = match served.get_mut(provider) {
                Some(entry) if entry.handle != Some(handle) => {
                    entry.handle = Some(handle);
                    true
                }
                _ => false,
            };
            drop(served);
            renewed
        };
        if renewed {
            answer_provider(&self.current(), handle, answers);
        }
        Ok(())
    }

    /// Tell the daemon `provider` offers what `catalogue` lists, answered with the handle its
    /// calls arrive on.
    async fn offer(
        &self,
        provider: &str,
        catalogue: &CataloguePage,
    ) -> Result<Handle, NotAvailable> {
        offer_on(&self.current(), provider, catalogue).await
    }
}

/// Hand `client` the answers of `provider`, which the daemon reaches under `handle`: each call
/// the dispatch admits reaches `answers` as the caller it admitted it for.
pub(crate) fn answer_provider(
    client: &crate::CollabClient,
    handle: Handle,
    answers: Arc<dyn ActionProvider + Send + Sync>,
) {
    client.client.answer(
        crate::wire::client::Answering::Served(handle),
        Arc::new(move |ask| {
            let answers = Arc::clone(&answers);
            Box::pin(async move { serve(&answers, ask).await })
        }),
    );
}

/// Offer `provider` with `catalogue` on `client`, answered with the handle its calls arrive on.
pub(crate) async fn offer_on(
    client: &crate::CollabClient,
    provider: &str,
    catalogue: &CataloguePage,
) -> Result<Handle, NotAvailable> {
    let catalogue = serde_json::to_string(catalogue)
        .map_err(|error| NotAvailable::Refused(error.to_string()))?;
    match client
        .client
        .call(crate::wire::Operation::Shell(ShellCall::Provide {
            provider: provider.to_owned(),
            catalogue,
        }))
        .await
        .map_err(super::failure)?
    {
        crate::wire::Reply::Shell(ShellReply::Providing(handle)) => Ok(handle),
        other => Err(NotAvailable::Refused(format!(
            "the daemon answered a provider with {other:?}"
        ))),
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
    let answer = tilo_action_calls::serve(answers, &caller, message)
        .ok_or("a provider answers requests, and that frame is none")?
        .await
        .ok_or("the provider dropped the call")?;
    RemoteActionFrame::new(answer)
        .encode()
        .map(Answer::Action)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "shell_dispatch_tests.rs"]
mod tests;

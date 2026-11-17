//! What the desktop's session asks of the page for its windows: showing
//! what an open named in the window it opened, asking the workspace about
//! nodes of its tree (what a share grants, what the person may change), and
//! reading what another app keeps ([`record_reads`]).

use super::{Desktop, Mount};

pub(super) mod record_calls;
mod record_reads;

/// The rows the page asks to run in an app's own window, which is not up yet: search's own rows
/// beside the ones a window asks of another app. Each runs in that window once it is up, as a
/// window's ask does ([`Mount::reveal_opened`]).
#[derive(Clone, Default)]
pub(crate) struct Reveals(
    std::rc::Rc<std::cell::RefCell<Vec<tilo_desktop_surface::session::Reveal>>>,
);

impl Reveals {
    /// Ask for `reveal` to run in its app's window once the window is up.
    pub(crate) fn ask(&self, reveal: tilo_desktop_surface::session::Reveal) {
        self.0.borrow_mut().push(reveal);
    }

    /// The asks made since this was last taken.
    fn take(&self) -> Vec<tilo_desktop_surface::session::Reveal> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

/// What the desktop's session asked of the page and is not done yet.
pub(super) struct Asks {
    /// What the windows it opened are to show, each until it is shown or
    /// gives up waiting for its window.
    reveals: std::cell::RefCell<Vec<Opening>>,
    /// The rows the page's search asks to run in an app's window, before they wait for it.
    asked: Reveals,
    /// The peer replicating the workspace, through whose workspaces API the windows'
    /// asks of nodes of the workspace's tree go.
    peer: Option<u32>,
    /// Nodes the workspace answered whether this person may change, waiting to
    /// reach the mounts' permissions.
    permitted: std::sync::Arc<std::sync::Mutex<Vec<(String, bool)>>>,
    /// The actions a window asked of another app that still run there, each with what tells the
    /// window that asked how it ended.
    running: std::cell::RefCell<
        Vec<(
            tilo_actions::InvocationId,
            tilo_desktop_surface::session::RevealAnswer,
        )>,
    >,
    /// The record reads of the windows: what another app keeps, read from what this page holds as
    /// its person may read it.
    records: record_reads::Reads,
}

impl Asks {
    /// What the session whose desktop end is `openings` asks of a page that replicates its
    /// workspace through `peer` and keeps the records no window has open in `store`.
    pub(super) fn new(
        peer: Option<u32>,
        openings: &tilo_desktop_surface::session::Openings,
        store: std::rc::Rc<dyn durable_store::DurableStore>,
    ) -> Self {
        Self {
            reveals: std::cell::RefCell::default(),
            asked: Reveals::default(),
            peer,
            permitted: std::sync::Arc::default(),
            running: std::cell::RefCell::default(),
            records: record_reads::Reads::new(openings, store),
        }
    }

    /// The queue the page's own rows are asked on, for the host that carries them here.
    pub(super) fn reveals(&self) -> Reveals {
        self.asked.clone()
    }

    /// The peer replicating the workspace, if the page has one.
    pub(super) fn peer(&self) -> Option<u32> {
        self.peer
    }

    /// The action `reveal` named started in its window as the job `job`: whoever waits for it
    /// hears its output now, or how it ends once it has.
    fn started(
        &self,
        reveal: &tilo_desktop_surface::session::Reveal,
        job: tilo_actions::InvocationId,
        started: ui::host_operation::ResourceOperationStart,
    ) {
        let Some(answer) = &reveal.answer else {
            return;
        };
        match started {
            ui::host_operation::ResourceOperationStart::Completed(output) => {
                answer.completed(output);
            }
            // It runs on: how it ends is the answer.
            ui::host_operation::ResourceOperationStart::Accepted => {
                self.running.borrow_mut().push((job, answer.clone()));
            }
        }
    }
}

impl Desktop {
    /// Ask the workspace what the windows asked of nodes of its tree: whether
    /// they may change one, or to grant (or take back) what a share of it
    /// gives. The workspace decides each, as it decides every write the grant
    /// then covers.
    pub(super) fn ask_host_of_nodes(&self) {
        let permitted = std::mem::take(
            &mut *self
                .asks
                .permitted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        if let Some(peer) = self.asks.peer {
            for (node, may) in permitted {
                crate::replicated::permit(peer, &node, may);
            }
        }
        for ask in self.openings.take_node_asks() {
            let Some(peer) = self.asks.peer else {
                ask.answer(Err("This page shares through no workspace".into()));
                continue;
            };
            let node = ask.node.clone();
            let action = ask.action.clone();
            let slot = std::sync::Arc::new(std::sync::Mutex::new(Some(ask)));
            let answered = std::sync::Arc::clone(&slot);
            let permitted = std::sync::Arc::clone(&self.asks.permitted);
            let woken = std::sync::Arc::clone(&self.shared.woken);
            let asked_node = node.clone();
            let sent = crate::replicated::host_then(
                peer,
                Box::new(|workspace| node_request(workspace, node, action)),
                Box::new(move |reply| {
                    // What the workspace permits beyond the person's role (an
                    // item shared with them to edit) reaches their windows.
                    if let Ok(tilo_relay_protocol::workspaces::WorkspacesReply::Permits(may)) =
                        &reply
                    {
                        permitted
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push((asked_node, *may));
                        woken.store(true, std::sync::atomic::Ordering::Release);
                    }
                    let ask = answered
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    if let Some(ask) = ask {
                        ask.answer(node_answer(reply));
                    }
                }),
            );
            if let Err(error) = sent
                && let Some(ask) = slot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
            {
                ask.answer(Err(error));
            }
        }
    }
}

/// The workspaces request asking workspace `workspace` `action` of `node`.
fn node_request(
    workspace: &str,
    node: String,
    action: tilo_desktop_surface::session::NodeAction,
) -> tilo_relay_protocol::workspaces::WorkspacesRequest {
    use tilo_desktop_surface::session::NodeAction;
    use tilo_relay_protocol::workspaces::WorkspacesRequest;
    let workspace = workspace.to_owned();
    match action {
        NodeAction::Share { share, access } => WorkspacesRequest::Share {
            workspace,
            share,
            node,
            access,
        },
        NodeAction::Unshare { share } => WorkspacesRequest::Unshare { workspace, share },
        NodeAction::Permits => WorkspacesRequest::Permits { workspace, node },
    }
}

/// A node ask's output from the workspace's reply.
fn node_answer(
    reply: Result<tilo_relay_protocol::workspaces::WorkspacesReply, String>,
) -> Result<serde_json::Value, String> {
    use tilo_relay_protocol::workspaces::WorkspacesReply;
    match reply? {
        WorkspacesReply::Permits(may) => Ok(serde_json::json!({ "undoable": may })),
        WorkspacesReply::Shared(count) => Ok(serde_json::json!({ "grants": count })),
        WorkspacesReply::Refused(reason) => Err(reason),
        other => Err(format!("the workspace answered something else: {other:?}")),
    }
}

/// How long an opened window has to come up and show what its open named.
const REVEAL_WAIT: ui::time::Duration = ui::time::Duration::from_secs(20);

/// An open a window was asked to show, and how waiting for it went.
struct Opening {
    reveal: tilo_desktop_surface::session::Reveal,
    /// When waiting for it ends: an app that still offers nothing by then is
    /// told so, once.
    until: ui::time::Instant,
    /// How many resources the window's app had published when the open was
    /// last tried: the row it names only arrives with a publication, so
    /// asking again before the app publishes would find the same.
    tried_at: Option<u64>,
}

impl Opening {
    /// Whether it still waits for its window at `now`. One whose wait ended tells the window that
    /// asked, if one did, that nothing took the action: it never reads as done.
    fn waits(&self, now: ui::time::Instant) -> bool {
        if self.until > now {
            return true;
        }
        if let Some(answer) = &self.reveal.answer {
            answer.refused(tilo_actions::remote::ActionHostError::Unavailable {
                reason: "The app did not take that in time".into(),
            });
        }
        false
    }
}

/// Whether `error` says the window offers nothing at that address yet (it is still coming up),
/// which is no refusal: the action is asked again when the window publishes.
fn coming(error: &tilo_actions::ApiError) -> bool {
    match error {
        tilo_actions::ApiError::Unknown(_) => true,
        tilo_actions::ApiError::Unavailable { code, .. } => code == "resource_unavailable",
        _ => false,
    }
}

/// The window's refusal `error`, as the window that asked hears it from its session.
fn refusal(error: tilo_actions::ApiError) -> tilo_actions::remote::ActionHostError {
    use tilo_actions::ApiError;
    use tilo_actions::remote::ActionHostError;
    let said = error.to_string();
    match error {
        ApiError::ConfirmationRequired => ActionHostError::confirmation_required(),
        ApiError::Unknown(_) => ActionHostError::UnknownAction,
        ApiError::Unavailable { code, .. }
            if code == "permission_denied" || code == "not_permitted" =>
        {
            ActionHostError::NotPermitted
        }
        ApiError::Unavailable { reason, .. } => ActionHostError::Unavailable { reason },
        ApiError::Refused { reason, .. } => ActionHostError::Refused { reason },
        ApiError::MissingArgument(_)
        | ApiError::UnexpectedArgument(_)
        | ApiError::InvalidArguments(_)
        | ApiError::ArgumentOutOfRange { .. } => ActionHostError::InvalidRequest { reason: said },
        ApiError::UnregisteredType(_) | ApiError::Failed(_) => {
            ActionHostError::Refused { reason: said }
        }
    }
}

impl Mount {
    /// Show, in each window the session opened on a record, the resource
    /// its open named, through the app's own `open` anchored on it, as a
    /// search hit opens it: a note in a notebook rather than the notebook, a
    /// Settings page rather than Settings' first one. Any other
    /// action a window asked of an app's resource is invoked in its window
    /// the same way.
    ///
    /// A window still coming up (its app's code arriving, its record
    /// syncing) publishes nothing yet: an open that found nothing is kept,
    /// and asked again when the app publishes next, not on every frame. The
    /// page is woken by that publication like it is woken by any message.
    pub(super) fn reveal_opened(&mut self) {
        self.answer_ended_calls();
        let Some(desktop) = &self.desktop else {
            return;
        };
        // What a window reads of another app's records, and each read held for a write that
        // landed since: nothing where no window reads any.
        desktop.answer_record_reads(self.id);
        let now = ui::time::now();
        let published = self.frame.published();
        let mut openings = desktop.asks.reveals.borrow_mut();
        openings.extend(
            desktop
                .openings
                .take_reveals()
                .into_iter()
                .chain(desktop.asks.asked.take())
                .filter(|reveal| {
                    // An open of the app's own resource is its window, unless
                    // it names where in the app to show. One the page does not
                    // run is dropped, which answers whoever asked: it opened.
                    crate::reveal::asks_window(
                        &reveal.action,
                        crate::desktop::Shared::narrower(
                            &reveal.app,
                            &reveal.record,
                            &reveal.resource,
                        ),
                        &reveal.arguments,
                    )
                })
                .map(|reveal| Opening {
                    reveal,
                    until: now.checked_add(REVEAL_WAIT).unwrap_or(now),
                    tried_at: None,
                }),
        );
        if openings.is_empty() {
            return;
        }
        let frame = &mut self.frame;
        openings.retain_mut(|opening| {
            let reveal = &opening.reveal;
            // Its window is not up yet: the window coming up is the page's
            // own news, and nothing here can be asked of it before that.
            let Some(tab) = desktop
                .shared
                .tab_on(&desktop.handle, &reveal.app, &reveal.record)
            else {
                return opening.waits(now);
            };
            // Nothing was published since the last try: the row this open
            // names is not there, and asking again would find the same.
            if opening.tried_at == Some(published) {
                return opening.waits(now);
            }
            // An `open` is the app's own, anchored on what it shows.
            let roots = frame
                .bound_resources_of(Some(tab), None)
                .unwrap_or_default();
            let Some((address, arguments)) =
                crate::reveal::address(&reveal.resource, &reveal.action, &reveal.arguments, &roots)
            else {
                opening.tried_at = Some(published);
                return opening.waits(now);
            };
            let address = ui::bound_dispatch::BoundActionAddress {
                instance_id: tab,
                address,
            };
            // Run as the person at this page, as their own press of it is: an action that asks
            // first is refused until they confirm it.
            match frame.start_bound_job(&address, &arguments, false) {
                Ok((job, started)) => {
                    desktop.asks.started(reveal, job, started);
                    false
                }
                // The window's own refusal is the answer of whoever waits for one.
                Err(error) if reveal.answer.is_some() && !coming(&error) => {
                    if let Some(answer) = &reveal.answer {
                        answer.refused(refusal(error));
                    }
                    false
                }
                Err(error) => {
                    if opening.until <= now {
                        web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(&format!(
                            "an opened window did not show {}: {error}",
                            reveal.resource
                        )));
                        return opening.waits(now);
                    }
                    opening.tried_at = Some(published);
                    true
                }
            }
        });
    }

    /// Tell each window that asked another app for an action how it ended there, once it has.
    fn answer_ended_calls(&self) {
        let Some(desktop) = &self.desktop else {
            return;
        };
        desktop.asks.running.borrow_mut().retain(|(job, answer)| {
            let ended = self
                .frame
                .job_updates(*job)
                .iter()
                .rev()
                .find(|update| update.is_terminal())
                .cloned();
            match ended {
                Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => {
                    answer.completed(output);
                }
                Some(tilo_actions::InvocationUpdate::Failed { error, .. }) => answer.failed(error),
                Some(tilo_actions::InvocationUpdate::Cancelled { message, .. }) => {
                    answer.cancelled(message);
                }
                _ => return true,
            }
            false
        });
    }
}

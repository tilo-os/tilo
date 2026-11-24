//! Delivering an invocation to the connection of an app that takes it, launching the app first
//! when none does: what every host runs to hand an app the action it was asked.
//!
//! Which connections publish what, and which take invocations, is [`Publishers`]. Admission stays
//! the host's own ([`DeliveryHost`]): this only waits for a publisher and hands it over.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::ActionAddress;
use tilo_actions::remote::{APPLICATION_OPEN_ACTION, ActionCallError, Caller, Completer};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};

use crate::published::PublishedSurface;
use crate::services::{LauncherServices, WindowlessStart};

/// What each live connection publishes and whether it takes invocations, by peer id. `E` is what
/// the host hands a connection's invocation stream.
pub struct Publishers<E> {
    /// Latest complete action surface per live connection, kept in the order the connections
    /// came: the catalogue lists them in it, so a caller is offered the same list each time it
    /// asks.
    pub surfaces: Mutex<BTreeMap<u64, PublishedSurface>>,
    pub streams: Mutex<HashMap<u64, mpsc::UnboundedSender<E>>>,
    /// The connected apps the host runs with no window, by peer id: they take actions on their
    /// records, never an `open`.
    pub windowless: Mutex<HashSet<u64>>,
    /// Monotonic generation for retained action projections: a surface change moves it, a
    /// stream change only notifies. A delivery waiting for its app wakes on either.
    pub changes: watch::Sender<u64>,
    /// The workspace and app of each launch that runs: an app starts once for the asks waiting.
    launching: Mutex<HashSet<(String, String)>>,
}

impl<E> Default for Publishers<E> {
    fn default() -> Self {
        Self {
            surfaces: Mutex::new(BTreeMap::new()),
            streams: Mutex::new(HashMap::new()),
            windowless: Mutex::new(HashSet::new()),
            changes: watch::Sender::new(0),
            launching: Mutex::new(HashSet::new()),
        }
    }
}

/// `peer_id`'s stream among `streams` while it takes invocations: one its app ended is gone from
/// them, and one nobody reads any more is closed.
pub fn open_stream<E, S: std::hash::BuildHasher>(
    streams: &HashMap<u64, mpsc::UnboundedSender<E>, S>,
    peer_id: u64,
) -> Option<&mpsc::UnboundedSender<E>> {
    streams.get(&peer_id).filter(|sender| !sender.is_closed())
}

impl<E> Publishers<E> {
    /// Wake every launch waiting for its app's invocation stream after one opened or closed.
    /// Retained projections do not depend on streams, so the generation stays.
    pub fn streams_changed(&self) {
        self.changes.send_modify(|_| {});
    }

    pub fn is_windowless(&self, peer_id: u64) -> bool {
        self.windowless
            .lock()
            .is_ok_and(|windowless| windowless.contains(&peer_id))
    }

    /// Whether peer `peer_id` may take an invocation of `action`: an instance with no window
    /// takes every action but `open`, which shows what it opens and so goes to a window.
    pub fn shows(&self, peer_id: u64, action: &str) -> bool {
        action != APPLICATION_OPEN_ACTION || !self.is_windowless(peer_id)
    }

    /// The running instance of `app_id` in `workspace` that may take `action` and takes
    /// invocations now: what an app's `open` or `start` answers with once one runs.
    pub fn running_instance(&self, app_id: &str, workspace: &str, action: &str) -> Option<u64> {
        let streams = self.streams.lock().ok()?;
        self.surfaces
            .lock()
            .ok()?
            .values()
            .filter(|surface| {
                surface.live
                    && self.shows(surface.peer_id, action)
                    && surface.app_id == app_id
                    && surface.route.cluster_id == workspace
                    && open_stream(&streams, surface.peer_id).is_some()
                    && !surface.actions.is_empty()
            })
            .map(|surface| surface.peer_id)
            .min()
    }

    /// The ready publisher of `address` and the address it publishes it under: the exact one,
    /// else (when `aliases`) one whose resource is a bound alias of `address`'s resource, or the
    /// one alias of the same resource now when `address` names an alias an instance that ended
    /// published ([`PublishedSurface::current_resource`]). An action that `shows` something
    /// needs a window to show it in.
    pub fn publisher_of(
        &self,
        (app_id, workspace, record): (&str, &str, Option<&str>),
        address: &ActionAddress,
        aliases: bool,
        shows: bool,
    ) -> Option<(u64, ActionAddress)> {
        let streams = self.streams.lock().ok()?;
        let surfaces = self.surfaces.lock().ok()?;
        surfaces
            .values()
            .filter(|surface| {
                surface.live
                    && self.shows(surface.peer_id, &address.action)
                    && !(shows && self.is_windowless(surface.peer_id))
                    && surface.app_id == app_id
                    && surface.route.cluster_id == workspace
                    && record.is_none_or(|record| surface.route.document_id == record)
                    && open_stream(&streams, surface.peer_id).is_some()
            })
            .filter_map(|surface| {
                let resource = if aliases {
                    surface
                        .current_resource(&address.resource, &address.action)
                        .or_else(|| {
                            surface
                                .actions
                                .iter()
                                .find(|action| {
                                    action.action == address.action
                                        && tilo_actions::selection::logical_resource_id(
                                            &action.resource,
                                        ) == address.resource
                                })
                                .map(|action| action.resource.as_str())
                        })?
                } else {
                    surface
                        .actions
                        .iter()
                        .find(|action| {
                            action.action == address.action && action.resource == address.resource
                        })?
                        .resource
                        .as_str()
                };
                Some((
                    surface.peer_id,
                    ActionAddress::new(resource, &address.action),
                ))
            })
            .min_by_key(|(peer_id, _)| *peer_id)
    }

    /// What `app_id`'s connections in `workspace` publish of `address`'s action and why none is
    /// ready for it: said with a delivery that found none.
    pub fn publishers_seen(
        &self,
        app_id: &str,
        workspace: &str,
        address: &ActionAddress,
    ) -> String {
        let Ok(streams) = self.streams.lock() else {
            return "the invocation streams are unavailable".into();
        };
        let Ok(surfaces) = self.surfaces.lock() else {
            return "the published surfaces are unavailable".into();
        };
        let seen: Vec<String> = surfaces
            .values()
            .filter(|surface| surface.app_id == app_id && surface.route.cluster_id == workspace)
            .map(|surface| {
                let resources: Vec<String> = surface
                    .actions
                    .iter()
                    .filter(|action| action.action == address.action)
                    .map(|action| {
                        tilo_actions::selection::logical_resource_id(&action.resource).into_owned()
                    })
                    .collect();
                format!(
                    "connection {} on {}: {}{}{}publishes \"{}\" on [{}]",
                    surface.peer_id,
                    surface.route.document_id,
                    if surface.live { "" } else { "closed, " },
                    if open_stream(&streams, surface.peer_id).is_some() {
                        ""
                    } else {
                        "takes no invocations, "
                    },
                    if self.shows(surface.peer_id, &address.action) {
                        ""
                    } else {
                        "has no window for it, "
                    },
                    address.action,
                    resources.join(", ")
                )
            })
            .collect();
        if seen.is_empty() {
            format!("no connection of {app_id} published in this workspace")
        } else {
            seen.join("; ")
        }
    }

    /// The connection ready to take `ask`'s address, and the address it takes it at: an instance
    /// of the app for a `start`, else the app's ready publisher of it.
    fn ready_for<O>(
        &self,
        ask: &DeliveryAsk<'_, O>,
        start: bool,
        shows: bool,
    ) -> Option<(u64, ActionAddress)> {
        if start {
            let peer_id = self.running_instance(ask.app_id, ask.workspace, &ask.address.action)?;
            return Some((peer_id, ask.address.clone()));
        }
        self.publisher_of(
            (ask.app_id, ask.workspace, ask.opened_on),
            ask.address,
            true,
            shows,
        )
    }

    /// Whether `app_id` has a connection in `workspace` that takes invocations: the app runs there.
    fn runs(&self, app_id: &str, workspace: &str) -> bool {
        let (Ok(streams), Ok(surfaces)) = (self.streams.lock(), self.surfaces.lock()) else {
            return false;
        };
        surfaces.values().any(|surface| {
            surface.live
                && surface.app_id == app_id
                && surface.route.cluster_id == workspace
                && open_stream(&streams, surface.peer_id).is_some()
        })
    }

    /// Hold the one launch of `app_id` in `workspace` while it runs: `None` while another ask's
    /// launch of it runs, which brings up the app this ask waits for. Ending it wakes the
    /// asks waiting.
    fn begin_launch(&self, app_id: &str, workspace: &str) -> Option<LaunchRunning<'_, E>> {
        let key = (workspace.to_owned(), app_id.to_owned());
        // The guard exists only once the launch is this ask's: dropping one built for a launch
        // that runs already would end that launch, under the lock its drop takes.
        if !self.launching.lock().ok()?.insert(key.clone()) {
            return None;
        }
        Some(LaunchRunning {
            publishers: self,
            key,
        })
    }
}

/// The launch of one app in one workspace that runs ([`Publishers::begin_launch`]).
struct LaunchRunning<'a, E> {
    publishers: &'a Publishers<E>,
    key: (String, String),
}

impl<E> Drop for LaunchRunning<'_, E> {
    fn drop(&mut self) {
        if let Ok(mut launching) = self.publishers.launching.lock() {
            launching.remove(&self.key);
        }
        self.publishers.streams_changed();
    }
}

/// One invocation to deliver to an app in a workspace, asked by `caller`: a launch or start it
/// causes acts as that caller, with its grants.
pub struct DeliveryAsk<'a, O> {
    pub caller: &'a Caller,
    /// Where the invocation came from, as the host admitted it.
    pub origin: Option<&'a O>,
    pub app_id: &'a str,
    pub workspace: &'a str,
    /// The resource the taking connection is opened on, where the invocation names one.
    pub opened_on: Option<&'a str>,
    /// The action it invokes, under the app's current ids.
    pub address: &'a ActionAddress,
    /// A workspace's `start` of the app: answered once the app runs.
    pub start: bool,
    /// How long the app has to offer the action once asked.
    pub timeout: Duration,
    /// The windowed launches that may run at once.
    pub launches: Arc<Semaphore>,
}

/// What a [`DeliveryHost`] answers asynchronously.
pub type DeliveryFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The host's admission half of one delivery: every check the invocation passes is its own.
pub trait DeliveryHost<O>: Send + Sync {
    /// Recheck `ask`'s original ownership: before each look while the delivery waits, and so
    /// before every launch and start it causes.
    ///
    /// # Errors
    /// Why the caller may no longer have it delivered.
    fn check_origin(&self, ask: &DeliveryAsk<'_, O>) -> Result<(), ActionCallError>;
    /// Whether `ask`'s action shows something, and so is taken only by a window of the app.
    fn shows(&self, ask: &DeliveryAsk<'_, O>) -> bool;
    /// Whether this host can open a window. An action that shows something is refused at once
    /// where it cannot and no window is ready, rather than waiting out the deadline.
    fn can_show(&self) -> bool {
        true
    }
    /// Hand `ask` to `peer_id`, which publishes it at `address` now, through the admission a live
    /// action takes: the job it started there. A `start` is answered with `peer_id`, the app's
    /// connection that runs now.
    fn deliver<'a>(
        &'a self,
        ask: &'a DeliveryAsk<'_, O>,
        peer_id: u64,
        address: ActionAddress,
    ) -> DeliveryFuture<'a, Result<u64, ActionCallError>>;
    /// Readmit `ask` and launch a window of its app for it as its caller, holding `permit` until
    /// the host call returns.
    ///
    /// # Errors
    /// Why the invocation may no longer launch the app.
    fn launch(
        &self,
        ask: &DeliveryAsk<'_, O>,
        permit: OwnedSemaphorePermit,
    ) -> Result<tokio::task::JoinHandle<Result<(), ActionCallError>>, ActionCallError>;
}

/// Launch `ask`'s app unless it already publishes the action, then deliver the invocation to the
/// connection that does, through `host`'s admission. An action that shows nothing starts the app
/// without a window where `launcher` can; only one that shows launches a window. One launch of an
/// app in a workspace runs at a time: the other asks of it wait for the app it brings up.
///
/// The wait wakes on what readiness depends on — a surface published or withdrawn, an invocation
/// stream opened or closed, in either order — on a launch of the app ending, on the delivery
/// deadline, on a start without a window ending, and on the caller dropping its answer.
///
/// # Errors
/// Why the invocation was not delivered.
pub async fn deliver_or_launch<E: Send, O: Sync, T: Send>(
    publishers: &Publishers<E>,
    launcher: Option<&dyn LauncherServices>,
    host: &dyn DeliveryHost<O>,
    ask: &DeliveryAsk<'_, O>,
    completer: &Completer<T>,
) -> Result<u64, ActionCallError> {
    let unavailable = |reason: String| ActionCallError::Unavailable { reason };
    let stopped_waiting = || unavailable("the caller stopped waiting for admission".into());
    let app_id = ask.app_id;
    let deadline = tokio::time::sleep(ask.timeout);
    let expired = || {
        unavailable(format!(
            "{app_id} was launched but did not offer \"{}\" within {:.1} s ({})",
            ask.address.action,
            ask.timeout.as_secs_f32(),
            publishers.publishers_seen(app_id, ask.workspace, ask.address),
        ))
    };
    let abandoned = completer.abandoned();
    tokio::pin!(deadline, abandoned);
    let shows = host.shows(ask);
    // Taken before the first look, so a publication between that look and the wait below still
    // wakes it.
    let mut changes = publishers.changes.subscribe();
    // This ask's launch or start of the app, kept until it is answered, and whether it ran.
    let (mut ours, mut ran) = (None, false);
    // The start of the instance with no window asked for, until it ends.
    let mut starting: Option<WindowlessStart> = None;
    loop {
        host.check_origin(ask)?;
        if completer.is_abandoned() {
            return Err(stopped_waiting());
        }
        if tokio::time::Instant::now() >= deadline.deadline() {
            return Err(expired());
        }
        if let Some(picked) = publishers.ready_for(ask, ask.start, host.shows(ask)) {
            if ask.start {
                return host.deliver(ask, picked.0, picked.1).await;
            }
            if let Some(delivered) = deliver_picked(publishers, host, ask, picked).await {
                return delivered;
            }
            continue;
        }
        if !ran && shows && !host.can_show() {
            return Err(unavailable(format!(
                "\"{}\" of {app_id} needs a window, and none can open here",
                ask.address.action
            )));
        }
        if !ran && ours.is_none() {
            ours = publishers.begin_launch(app_id, ask.workspace);
        }
        // Another ask's launch of the app runs: wait for the app it brings up, and once that runs
        // without what this ask needs, ask the running app the way any launch does.
        let launch = !ran && (ours.is_some() || publishers.runs(app_id, ask.workspace));
        if launch && !shows {
            let started = launcher.map_or(Ok(None), |launcher| {
                launcher.windowless(app_id, ask.workspace, ask.opened_on)
            });
            if let Some(start) = started.map_err(|why| not_started(app_id, &why))? {
                (starting, ran) = (Some(start), true);
                continue;
            }
        }
        if launch {
            let permit = tokio::select! {
                biased;
                () = &mut abandoned => return Err(stopped_waiting()),
                () = &mut deadline => return Err(expired()),
                seen = changes.changed() => {
                    seen.map_err(|_| unavailable("the broker stopped".into()))?;
                    continue;
                }
                permit = Arc::clone(&ask.launches).acquire_owned() => {
                    permit.map_err(|_| unavailable("the launch queue stopped".into()))?
                }
            };
            // Ownership read again after the launch queue's wait.
            let mut launch = host
                .check_origin(ask)
                .and_then(|()| host.launch(ask, permit))?;
            let result = tokio::select! {
                biased;
                () = &mut abandoned => {
                    launch.abort();
                    return Err(stopped_waiting());
                }
                () = &mut deadline => {
                    launch.abort();
                    return Err(expired());
                }
                result = &mut launch => result,
            };
            result.map_err(|error| {
                unavailable(format!("{app_id} launch worker stopped: {error}"))
            })??;
            ran = true;
            continue;
        }
        tokio::select! {
            seen = changes.changed() => {
                seen.map_err(|_| unavailable("the broker stopped".into()))?;
            }
            () = &mut deadline => return Err(expired()),
            () = &mut abandoned => return Err(stopped_waiting()),
            // A start that failed is the action's answer now, with why.
            failed = start_failed(&mut starting, app_id) => return Err(failed),
        }
    }
}

/// Deliver `ask` to `picked`, the connection found ready for it and the address it takes it at.
/// `None` when that connection stopped taking invocations before the admission, as a window that
/// closed ends its stream: a change like any other, after which the caller looks again and passes
/// it over.
pub async fn deliver_picked<E, O: Sync>(
    publishers: &Publishers<E>,
    host: &dyn DeliveryHost<O>,
    ask: &DeliveryAsk<'_, O>,
    (peer_id, published): (u64, ActionAddress),
) -> Option<Result<u64, ActionCallError>> {
    let delivered = host.deliver(ask, peer_id, published).await;
    let still_picked = || {
        let again = publishers.ready_for(ask, false, host.shows(ask));
        again.is_some_and(|(again, _)| again == peer_id)
    };
    (delivered.is_ok() || still_picked()).then_some(delivered)
}

fn not_started(app_id: &str, why: &str) -> ActionCallError {
    ActionCallError::Unavailable {
        reason: format!("{app_id} could not run without a window: {why}"),
    }
}

/// Why the start of an instance with no window failed, once it does; pending while there is none
/// to wait on, or once it runs. Cancel-safe: a wait dropped before the start ended leaves
/// `starting` as it was.
async fn start_failed(starting: &mut Option<WindowlessStart>, app_id: &str) -> ActionCallError {
    let Some(start) = starting.as_mut() else {
        return std::future::pending().await;
    };
    let ended = start.await;
    *starting = None;
    match ended {
        Ok(Ok(())) => std::future::pending().await,
        Ok(Err(why)) => not_started(app_id, &why),
        Err(error) => {
            tracing::warn!(%error, app = app_id, "starting the app without a window did not finish");
            not_started(app_id, &format!("the host stopped starting it ({error})"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host with no screen, which refuses whatever reaches it.
    struct NoScreen;

    impl DeliveryHost<()> for NoScreen {
        fn check_origin(&self, _: &DeliveryAsk<'_, ()>) -> Result<(), ActionCallError> {
            Ok(())
        }
        fn shows(&self, _: &DeliveryAsk<'_, ()>) -> bool {
            true
        }
        fn can_show(&self) -> bool {
            false
        }
        fn deliver<'a>(
            &'a self,
            _: &'a DeliveryAsk<'_, ()>,
            _: u64,
            _: ActionAddress,
        ) -> DeliveryFuture<'a, Result<u64, ActionCallError>> {
            Box::pin(std::future::ready(Err(ActionCallError::NotPermitted)))
        }
        fn launch(
            &self,
            _: &DeliveryAsk<'_, ()>,
            _: OwnedSemaphorePermit,
        ) -> Result<tokio::task::JoinHandle<Result<(), ActionCallError>>, ActionCallError> {
            Err(ActionCallError::NotPermitted)
        }
    }

    #[tokio::test]
    async fn an_action_that_shows_is_refused_at_once_where_no_window_can_open() {
        let caller = Caller::local_person("owner").in_workspace("home");
        let address = ActionAddress::new("files.app", "open");
        let ask = DeliveryAsk {
            caller: &caller,
            origin: None,
            app_id: "files",
            workspace: "home",
            opened_on: None,
            address: &address,
            start: false,
            timeout: Duration::from_secs(3600),
            launches: Arc::new(Semaphore::new(1)),
        };
        let (completer, _answer) = tilo_actions::remote::pending::<()>();
        let publishers = Publishers::<()>::default();
        match deliver_or_launch(&publishers, None, &NoScreen, &ask, &completer).await {
            Err(ActionCallError::Unavailable { reason }) => {
                assert!(reason.contains("needs a window"), "{reason}");
            }
            other => panic!("expected the refusal, got {other:?}"),
        }
    }
}

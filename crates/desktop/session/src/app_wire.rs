//! The session's side of the app wire: the resources its services publish,
//! read, watched and invoked by the apps it runs.
//!
//! An app is a process of its own, and the socket it speaks to its session on
//! is handed to the device's daemon ([`crate::app_socket`]), which admits the
//! app and keeps the workspaces the apps share. What this session adds is one
//! provider: what its services publish ([`Link`], as
//! [`PublishedResources`]), served to the daemon on the shell link
//! ([`crate::seat_services`]). Nothing of the wire itself is served here.
//!
//! The two sides run on two threads. The daemon's calls arrive on the session's
//! runtime; the services belong to the session's loop, the only thread that may
//! touch them, because a service blocks on its device and keeps its worker.
//! [`Link`] is the service: an app's call becomes an [`Ask`] the loop answers on
//! its next pass, and a watched resource's reading moves to the apps watching it
//! there.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime_collab::wire::{Event, Receipt};
use serde_json::Map;
use tilo_actions::ApiError;
use tilo_app_host::services::{Caller, PublishedResources, Reading, ResourceWatch};
use tilo_lock::MutexRecover as _;
use tilo_service::{Services, Started, Wake};
use tokio::sync::{mpsc, oneshot};

use crate::launches::Launches;

/// A watch the loop keeps: the resource an app connection watches, and what
/// that connection was last sent.
struct Watch {
    id: ResourceWatch,
    /// The app and workspace reading it: a device resource that answers for its reader is read
    /// as theirs.
    caller: Caller,
    resource: String,
    /// The services' revision this watch was last served at.
    revision: Option<u64>,
    /// The catalogue revision, at which it was last told no service publishes
    /// the resource: what it asks again after a service was added, and never
    /// at every reading of one it does publish.
    missing: Option<u64>,
    /// What it was last sent, so a change elsewhere does not re-send it.
    reading: Option<Vec<u8>>,
    /// Where its readings go.
    sink: mpsc::UnboundedSender<Event>,
}

/// What a connection asked, waiting for the loop's pass.
enum Ask {
    Snapshot {
        caller: Caller,
        resource: String,
        reply: oneshot::Sender<Result<Reading, String>>,
    },
    Invoke {
        caller: Caller,
        resource: String,
        action: String,
        /// The arguments, as the JSON object the wire carried.
        arguments: Vec<u8>,
        reply: oneshot::Sender<Receipt>,
    },
}

/// The service between the session's loop and the broker's connections: what an
/// app asks of the services, and the readings the apps watch.
pub struct Link {
    asks: Mutex<Vec<Ask>>,
    watches: Mutex<Vec<Watch>>,
    next_watch: AtomicU64,
    /// The readings that are the person's own, by resource id, as the services published them:
    /// only the session's own programs read those.
    personal: Mutex<Personal>,
    /// The apps this session started, as the broker's connections are admitted by them: an app
    /// the session runs itself, without a package's sandbox, is its own code. Set once the broker
    /// serves the link.
    launches: std::sync::OnceLock<std::sync::Arc<Launches>>,
    /// Where the Workspace package's own actions run: the device's dispatch, which runs a
    /// workspace's governance where the workspace keeps it. Set once the daemon gave this
    /// session its dispatch.
    dispatch: Mutex<Option<std::sync::Arc<dyn WorkspaceActions>>>,
    wake: Wake,
}

/// Where a workspace's own actions ([`crate::app_wire`]'s `workspace/content` and the rest of
/// the Workspace package's roots) run: the device's dispatch, which runs each where the
/// workspace's governance publishes it, on whichever device holds it.
pub(crate) trait WorkspaceActions: Send + Sync {
    /// Run `request` in `workspace`: what it answered, told once it ended.
    fn follow(
        &self,
        workspace: String,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<Result<tilo_actions::ActionOutput, String>>;
}

impl WorkspaceActions for runtime_collab::ShellDispatch {
    fn follow(
        &self,
        workspace: String,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<Result<tilo_actions::ActionOutput, String>> {
        // The answer arrives on the dispatch's own runtime, told once the action ended.
        runtime_collab::ShellDispatch::follow(self, Some(workspace), request)
    }
}

/// [`Link::personal`], with the services' catalogue it was read at: what a service publishes moves
/// only when a service is added.
#[derive(Default)]
struct Personal {
    catalogue: Option<u64>,
    readings: std::collections::BTreeSet<String>,
}

impl Personal {
    /// Read the personal readings out of `resources`, every resource under them included.
    fn read(catalogue: u64, resources: &[tilo_actions::ResourceSpec]) -> Self {
        fn collect(
            into: &mut std::collections::BTreeSet<String>,
            resource: &tilo_actions::ResourceSpec,
        ) {
            if resource.personal {
                into.insert(resource.id.as_str().to_owned());
            }
            for child in &resource.children {
                collect(into, child);
            }
        }
        let mut readings = std::collections::BTreeSet::new();
        for resource in resources {
            collect(&mut readings, resource);
        }
        Self {
            catalogue: Some(catalogue),
            readings,
        }
    }
}

impl Link {
    /// The apps this session started, which the connections it takes are admitted by.
    pub(crate) fn admitting(&self, launches: std::sync::Arc<Launches>) {
        let _ = self.launches.set(launches);
    }

    /// A link that wakes the session's loop with `wake`.
    #[must_use]
    pub fn new(wake: Wake) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            asks: Mutex::new(Vec::new()),
            watches: Mutex::new(Vec::new()),
            next_watch: AtomicU64::new(1),
            personal: Mutex::new(Personal::default()),
            launches: std::sync::OnceLock::new(),
            dispatch: Mutex::new(None),
            wake,
        })
    }

    /// Where the Workspace package's own actions run: the device's dispatch, once the daemon
    /// handed this session one.
    pub(crate) fn dispatching(&self, dispatch: std::sync::Arc<dyn WorkspaceActions>) {
        *self.dispatch.lock_recover() = Some(dispatch);
    }

    /// Refuse `resource`'s reading to `caller` when it is the person's own and the caller is not
    /// a program this session runs itself: a package's app reads what it declares, not the
    /// person's clipboard or their notifications.
    fn reading_granted(&self, caller: &Caller, resource: &str) -> Result<(), String> {
        let personal = self.personal.lock_recover().readings.contains(resource);
        let own_program = self
            .launches
            .get()
            .is_none_or(|launches| launches.own_program(&caller.app_id));
        if personal && !own_program {
            return Err("caller is not granted this reading".to_owned());
        }
        Ok(())
    }

    fn ask(&self, ask: Ask) {
        self.asks.lock_recover().push(ask);
        (self.wake)();
    }

    /// Answer what the apps asked since the last pass, and send every
    /// watched resource's reading that moved: what the session's loop calls
    /// once per pass, after the services took in what their devices did.
    pub fn serve(&self, services: &mut Services) {
        self.read_personal(services);
        let asked = std::mem::take(&mut *self.asks.lock_recover());
        for ask in asked {
            match ask {
                Ask::Snapshot {
                    caller,
                    resource,
                    reply,
                } => self.answer_snapshot(services, &caller, &resource, reply),
                Ask::Invoke {
                    caller,
                    resource,
                    action,
                    arguments,
                    reply,
                } => {
                    tracing::debug!(
                        app = caller.app_id,
                        workspace = caller.workspace,
                        %resource,
                        %action,
                        "an app invokes a published action"
                    );
                    // The Workspace package's own roots are the workspace's actions, which its
                    // governance publishes: they run where the workspace keeps them, through the
                    // device's dispatch, and no service of this session publishes them.
                    let Some(reply) =
                        self.workspace_action(&caller, &resource, &action, &arguments, reply)
                    else {
                        continue;
                    };
                    if services
                        .read_for(&caller.app_id, &caller.workspace, &resource)
                        .is_none()
                    {
                        let _ = reply.send(Receipt::Refused {
                            code: "refused".to_owned(),
                            message: format!("this session publishes no `{resource}` resource"),
                        });
                    } else {
                        answer(services, &caller, &resource, &action, &arguments, reply);
                    }
                }
            }
        }
        self.publish(services);
    }

    /// Run a call on the Workspace package's own root (`workspace/content`) as the workspace's
    /// action it names, on the device's dispatch, where the workspace's governance publishes it.
    /// `None` when it was one, the answer going to `reply`; `Some(reply)` when it was no such
    /// call, for the services to answer or refuse.
    fn workspace_action(
        &self,
        caller: &Caller,
        resource: &str,
        action: &str,
        arguments: &[u8],
        reply: oneshot::Sender<Receipt>,
    ) -> Option<oneshot::Sender<Receipt>> {
        let Some(child) = resource
            .strip_prefix("workspace/")
            .filter(|child| !child.is_empty() && !child.contains('/'))
        else {
            return Some(reply);
        };
        // A root the workspace's governance publishes an action on is the workspace's own; one
        // it publishes none on is no action this session answers.
        let address = tilo_actions::ActionAddress::new(
            tilo_actions::remote::workspace_governance_child(&caller.workspace, Some(child)),
            action,
        );
        let Some((workspace, operation)) =
            tilo_actions::remote::workspace_governance_operation(&address)
        else {
            return Some(reply);
        };
        let arguments = match serde_json::from_slice::<Map<String, serde_json::Value>>(arguments) {
            Ok(arguments) => arguments,
            Err(error) => {
                let _ = reply.send(Receipt::Refused {
                    code: "invalid_arguments".to_owned(),
                    message: format!("the arguments are not a JSON object: {error}"),
                });
                return None;
            }
        };
        let Some(dispatch) = self.dispatch.lock_recover().clone() else {
            let _ = reply.send(Receipt::Refused {
                code: "unavailable".to_owned(),
                message: format!(
                    "this device cannot reach `{resource}`: the session has no dispatch"
                ),
            });
            return None;
        };
        let request = tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget::workspace_governance(
                workspace, operation,
            ),
            argument: tilo_actions::remote::InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        dispatch
            .follow(caller.workspace.clone(), request)
            .then(move |answer| {
                let answer = answer
                    .unwrap_or_else(|| Err("the device's dispatch never answered".to_owned()));
                let _ = reply.send(match answer {
                    Ok(output) => receipt(Ok(output)),
                    Err(reason) => Receipt::Refused {
                        code: "refused".to_owned(),
                        message: reason,
                    },
                });
            });
        None
    }

    /// Answer a snapshot of `resource` for `caller`: from the services when one of them publishes
    /// it, and with no reading when none does.
    fn answer_snapshot(
        &self,
        services: &Services,
        caller: &Caller,
        resource: &str,
        reply: oneshot::Sender<Result<Reading, String>>,
    ) {
        match self.reading_granted(caller, resource) {
            Err(reason) => {
                let _ = reply.send(Err(reason));
            }
            Ok(()) => match services.read_for(&caller.app_id, &caller.workspace, resource) {
                Some(value) => {
                    let _ = reply.send(Ok((services.revision(), Some(encoded(&value)))));
                }
                None => {
                    let _ = reply.send(Ok((0, None)));
                }
            },
        }
    }

    /// Read the personal readings out of `services` again when a service was added.
    fn read_personal(&self, services: &Services) {
        let catalogue = services.catalogue_revision();
        let mut personal = self.personal.lock_recover();
        if personal.catalogue == Some(catalogue) {
            return;
        }
        *personal = Personal::read(catalogue, &services.resources());
    }

    /// Send the readings of the watched resources that moved.
    fn publish(&self, services: &Services) {
        let revision = services.revision();
        let catalogue = services.catalogue_revision();
        let mut watches = self.watches.lock_recover();
        watches.retain_mut(|watch| {
            if watch.revision == Some(revision) {
                return true;
            }
            let reading = services
                .read_for(
                    &watch.caller.app_id,
                    &watch.caller.workspace,
                    &watch.resource,
                )
                .map(|value| encoded(&value));
            watch.revision = Some(revision);
            if reading.is_none() {
                // No service publishes it yet: the app is told once, and
                // again only once a service was added, which is the only
                // thing that can make one publish it.
                if watch.missing == Some(catalogue) {
                    return true;
                }
                watch.missing = Some(catalogue);
            } else {
                watch.missing = None;
                if reading == watch.reading {
                    return true;
                }
            }
            watch.reading.clone_from(&reading);
            watch
                .sink
                .send(Event::Reading {
                    resource: watch.resource.clone(),
                    revision,
                    reading,
                })
                .is_ok()
        });
    }
}

/// The session's services, as the broker's connections read them: a resource
/// is named by its id, and what an app reaches is what the services publish.
impl PublishedResources for Link {
    fn snapshot(
        &self,
        caller: &Caller,
        resource: &str,
    ) -> oneshot::Receiver<Result<Reading, String>> {
        let (reply, answered) = oneshot::channel();
        self.ask(Ask::Snapshot {
            caller: caller.clone(),
            resource: resource.to_owned(),
            reply,
        });
        answered
    }

    fn watch(
        &self,
        caller: &Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<Event>,
    ) -> Result<ResourceWatch, String> {
        self.reading_granted(caller, resource)?;
        let id = ResourceWatch::new(self.next_watch.fetch_add(1, Ordering::Relaxed));
        self.watches.lock_recover().push(Watch {
            id,
            caller: caller.clone(),
            resource: resource.to_owned(),
            revision: None,
            missing: None,
            reading: None,
            sink,
        });
        (self.wake)();
        Ok(id)
    }

    fn unwatch(&self, id: ResourceWatch) {
        self.watches.lock_recover().retain(|watch| watch.id != id);
    }

    fn invoke(
        &self,
        caller: &Caller,
        resource: &str,
        action: &str,
        arguments: Vec<u8>,
    ) -> oneshot::Receiver<Receipt> {
        let (reply, answered) = oneshot::channel();
        self.ask(Ask::Invoke {
            caller: caller.clone(),
            resource: resource.to_owned(),
            action: action.to_owned(),
            arguments,
            reply,
        });
        answered
    }
}

/// A value as the wire carries it: its JSON.
fn encoded(value: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

/// Answer `reply` with what invoking one of the services' actions answered: at once, or when the
/// async work it started lands.
fn answer(
    services: &mut Services,
    caller: &Caller,
    resource: &str,
    action: &str,
    arguments: &[u8],
    reply: oneshot::Sender<Receipt>,
) {
    let arguments = match serde_json::from_slice::<Map<String, serde_json::Value>>(arguments) {
        Ok(arguments) => arguments,
        Err(error) => {
            let _ = reply.send(Receipt::Refused {
                code: "invalid_arguments".into(),
                message: format!("the arguments are not a JSON object: {error}"),
            });
            return;
        }
    };
    let done = match services.invoke_for(
        &caller.app_id,
        &caller.workspace,
        resource,
        action,
        &arguments,
    ) {
        Some(Ok(Started::Done(output))) => Ok(output),
        Some(Ok(Started::Accepted)) => {
            let _ = reply.send(Receipt::Accepted);
            return;
        }
        Some(Ok(Started::Later(later))) => {
            later.then(move |landed| {
                let _ = reply.send(receipt(landed.unwrap_or_else(|| {
                    Err(ApiError::Failed("the action's work was given up".into()))
                })));
            });
            return;
        }
        Some(Err(error)) => Err(error),
        None => {
            let _ = reply.send(Receipt::Refused {
                code: "unknown_action".into(),
                message: format!("this session publishes no `{resource}` resource"),
            });
            return;
        }
    };
    let _ = reply.send(receipt(done));
}

/// What an action that finished answered, as the wire carries it.
fn receipt(done: Result<tilo_actions::ActionOutput, ApiError>) -> Receipt {
    match done {
        Ok(output) => match serde_json::to_vec(&output) {
            Ok(output) => Receipt::Done(output),
            Err(error) => Receipt::Refused {
                code: "failed".into(),
                message: format!("the action's output does not encode: {error}"),
            },
        },
        Err(error) => Receipt::Refused {
            code: error.code().to_owned(),
            message: error.to_string(),
        },
    }
}

/// The app wire while the session serves it: what the apps ask of the services,
/// and the socket their connections reach the device's daemon through.
pub(crate) struct AppWire {
    pub(crate) link: std::sync::Arc<Link>,
    /// The socket the apps connect to: what the launcher hands an app, and
    /// what a surface app connects for itself.
    pub(crate) path: std::path::PathBuf,
    /// Where this session's apps connect, each connection handed to the device's daemon
    /// ([`crate::app_socket`]).
    pub(crate) _socket: crate::app_socket::AppSocket,
    /// The calls of the workspaces this session takes part in, as the loop
    /// heard them: what a voice channel shows.
    pub(crate) calls: std::sync::Arc<crate::calls::Calls>,
    /// The service apps join a call through, when this session carries
    /// calls at all.
    pub(crate) calls_service: Option<tilo_native_media::NativeMediaService>,
    /// The people's carets in the workspaces this session takes part in: the person here's as
    /// their apps publish it, and everyone else's as this session's apps show them.
    pub(crate) carets: crate::carets::Carets,
    /// Files' clipboard, whose copies the loop puts on the desktop's
    /// clipboard: what the person pastes into any other app.
    pub(crate) file_clipboard: std::sync::Arc<crate::file_clipboard::NativeFileClipboard>,
    /// What Browser's asks for vault items are answered with, shared with the service that
    /// answers them.
    pub(crate) item_lending: std::sync::Arc<crate::item_lending::ItemLending>,
    /// The device's one dispatch as this session's person, over the shell's link, when the
    /// device's daemon handed this session one.
    pub(crate) dispatch: Option<std::sync::Arc<runtime_collab::ShellDispatch>>,
    /// The providers of the person's seat as the daemon reaches them.
    pub(crate) seat: Option<crate::session_providers::SeatServed>,
    /// The app whose call the proxies were last told is live, so a change of call opens one app's
    /// UDP lane and closes the one before it.
    pub(crate) call_lane: Mutex<Option<String>>,
    /// The card of this device's call over the windows.
    pub(crate) call_card: Mutex<crate::call_card::CallCard>,
}

impl AppWire {
    /// What Browser's asks for vault items are answered with: a session that holds a
    /// workspace's Browser says so here, and names who asks its server.
    pub(crate) fn item_lending(&self) -> std::sync::Arc<crate::item_lending::ItemLending> {
        std::sync::Arc::clone(&self.item_lending)
    }

    /// Say this device's call where everyone in the workspace reads it, and take
    /// in what they say of theirs: the channel a person is in and who is in
    /// each channel's call, which is what this session's apps show of calls. What this device's
    /// call is named, the channel's name.
    pub(crate) fn follow_calls(
        &self,
        sync: &crate::sync::Client,
        published: impl FnOnce() -> Vec<tilo_actions::remote::CatalogueRow>,
    ) -> String {
        let own = self
            .calls_service
            .as_ref()
            .and_then(tilo_native_media::NativeMediaService::own_call);
        // A call's UDP lane belongs to the daemon: this loop tells each launch's proxy whether a
        // call of its app is live. The app never says so, and the proxy carries nothing outside
        // one.
        let lane = own.as_ref().map(|call| call.app.clone());
        {
            let mut previous = self
                .call_lane
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *previous != lane {
                let directory = launcher::dbus_proxy::session_runtime_dir().join("tilo/net");
                if let Some(app) = previous.take() {
                    launcher::egress_proxy::announce_calls(&directory, &app, false);
                }
                if let Some(app) = &lane {
                    launcher::egress_proxy::announce_calls(&directory, app, true);
                }
                *previous = lane;
            }
        }
        let name = own.as_ref().map_or_else(String::new, |call| {
            // What the app calls the channel, as it publishes it on the device's dispatch, so the
            // workspace reads a name and not an id; what the people in it said is the fallback.
            crate::calls::channel_name(&published(), (&call.workspace, &call.app), &call.channel)
                .unwrap_or_else(|| self.calls.name(&call.channel))
        });
        let presence = own.as_ref().map(|call| crate::sync::InCall {
            call: ui::CallPresence {
                channel: call.channel.clone(),
                name: name.clone(),
                bridge: None,
            },
            camera: call.camera,
            screen: call.screen,
            muted: call.muted,
            deafened: call.deafened,
        });
        sync.in_call(presence.clone());
        self.calls.follow(sync, presence.as_ref());
        name
    }

    /// What the session tells the workspace's apps of its own session: whose view this person
    /// follows, and the step of a move one window's app is asked for.
    pub(crate) fn follow_dispatcher(&self) -> Option<crate::window_follow::WindowFollow> {
        self.dispatch
            .clone()
            .map(crate::window_follow::WindowFollow::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An app watching a resource no service of this device publishes hears it once, and again
    /// only when a service is added: a machine without a battery does not tell it on every
    /// reading of another service.
    #[test]
    fn a_resource_no_service_publishes_is_told_once() {
        let link = Link::new(std::sync::Arc::new(|| {}));
        let (sink, mut heard) = mpsc::unbounded_channel();
        let caller = Caller {
            app_id: "so.tilo.quick-settings".to_owned(),
            workspace: String::new(),
        };
        let _watch = link.watch(&caller, "power:battery", sink);
        let mut services = Services::default();
        link.publish(&services);
        assert!(
            matches!(heard.try_recv(), Ok(Event::Reading { reading: None, .. })),
            "the app is told no service publishes it"
        );

        // Another service's reading moves: nothing is said of the battery
        // again, however many revisions go by.
        let counted = std::sync::Arc::new(std::sync::Mutex::new(1_u8));
        services.add(Box::new(Counted(std::sync::Arc::clone(&counted))));
        link.publish(&services);
        // The catalogue moved with the service added, so it is asked again
        // once, and that is all.
        assert!(
            matches!(heard.try_recv(), Ok(Event::Reading { reading: None, .. })),
            "a service was added, so it is asked again once"
        );
        *counted.lock().unwrap() = 2;
        services
            .invoke("test:counted", "bump", &Map::new())
            .expect("the service is published")
            .expect("the bump is taken");
        link.publish(&services);
        assert!(
            heard.try_recv().is_err(),
            "a reading of another service says nothing of the battery"
        );
    }

    use tilo_actions::ResourceSpec;

    /// A service publishing one reading the test moves.
    struct Counted(std::sync::Arc<std::sync::Mutex<u8>>);

    impl tilo_service::Service for Counted {
        fn resources(&self) -> Vec<ResourceSpec> {
            vec![
                ResourceSpec::new("test:counted", "test", "Counted").with_action(
                    tilo_actions::ActionSpec::typed_operation(
                        "bump",
                        "",
                        "",
                        tilo_actions::OperationKind::Effect,
                        "test.bump",
                        Vec::new(),
                        |_| {
                            tilo_actions::EncodedAction::encode_operation("test.bump", &())
                                .unwrap_or_else(|error| tilo_actions::EncodedAction {
                                    type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE
                                        .to_owned(),
                                    payload: error.to_string().into_bytes(),
                                })
                        },
                    ),
                ),
            ]
        }

        fn read(&self, resource: &str) -> Option<serde_json::Value> {
            (resource == "test:counted")
                .then(|| serde_json::json!({ "value": *self.0.lock().unwrap() }))
        }

        fn invoke(
            &mut self,
            _resource: &str,
            _action: &str,
            _: &Map<String, serde_json::Value>,
        ) -> Result<Started, ApiError> {
            Ok(Started::Done(tilo_actions::ActionOutput::default()))
        }
    }

    /// A dispatch that records what a workspace's own actions asked of it, and answers at once.
    #[derive(Default)]
    struct Dispatched(Mutex<Vec<(String, tilo_actions::remote::InvokeRequest)>>);

    impl WorkspaceActions for Dispatched {
        fn follow(
            &self,
            workspace: String,
            request: tilo_actions::remote::InvokeRequest,
        ) -> tilo_actions::remote::Pending<Result<tilo_actions::ActionOutput, String>> {
            self.0.lock().unwrap().push((workspace, request));
            let (done, answer) = tilo_actions::remote::pending();
            done.complete(Ok(tilo_actions::ActionOutput::default()));
            answer
        }
    }

    /// A window's call on the Workspace package's own root (`workspace/content`) is the
    /// workspace's action it names, sent to the device's dispatch at the workspace's governance
    /// address as the window's person: what Settings' Save does.
    #[test]
    fn a_windows_call_on_the_workspaces_content_is_the_workspaces_own_action() {
        let wake: Wake = std::sync::Arc::new(|| ());
        let link = Link::new(wake);
        let dispatch = std::sync::Arc::new(Dispatched::default());
        link.dispatching(std::sync::Arc::clone(&dispatch) as std::sync::Arc<dyn WorkspaceActions>);
        let (reply, mut answered) = oneshot::channel();
        link.ask(Ask::Invoke {
            caller: Caller {
                app_id: "so.tilo.settings".to_owned(),
                workspace: "team".to_owned(),
            },
            resource: "workspace/content".to_owned(),
            action: "set-policy".to_owned(),
            arguments: serde_json::to_vec(&serde_json::json!({
                "file_bytes": 25 * 1_048_576,
            }))
            .expect("the arguments encode"),
            reply,
        });
        link.serve(&mut Services::default());

        let sent = dispatch.0.lock().unwrap();
        let [(workspace, request)] = sent.as_slice() else {
            panic!("one action: {sent:?}");
        };
        assert_eq!(workspace, "team");
        assert_eq!(
            request.target.provider,
            tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER,
            "the workspace's own provider"
        );
        assert_eq!(
            request.target.address.resource,
            tilo_actions::remote::workspace_governance_child("team", Some("content")),
            "the workspace's own content, at its governance's address"
        );
        assert_eq!(request.target.address.action, "set-policy");
        assert_eq!(
            request.argument,
            tilo_actions::remote::InvokeArgument::Json(serde_json::Map::from_iter([(
                "file_bytes".to_owned(),
                serde_json::json!(25 * 1_048_576)
            )]))
        );
        drop(sent);
        assert!(
            matches!(answered.try_recv(), Ok(Receipt::Done(_))),
            "the window reads the answer the workspace gave"
        );
    }
}

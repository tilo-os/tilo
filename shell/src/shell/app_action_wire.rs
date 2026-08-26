//! What the applications on this desktop can do, and how something that is
//! not one of them asks for it.
//!
//! [`app_resources`](crate::shell::search::providers::app_resources) already
//! serves an app's own resource tree into the shell's catalogue, and until now
//! it could only reach a *widget* — an app compiled into the compositor. Every
//! packaged qOS app is a separate process, so on a real desktop that provider
//! published an empty tree every time: the palette offered a hundred-odd
//! operations and not one of them came from a running application.
//!
//! This module is the other half. Applications publish their surface over the
//! socket they already hold (`publishActions`, see
//! `crates/runtime/runtime-collab/schema/collab.capnp`); this holds what they
//! published, hands it to the same provider, and carries an invocation back to
//! the one connection that can perform it.
//!
//! ## The app is what runs it
//!
//! An operation is not moved here to be executed. `qos_actions::door::invoke`
//! needs the live resource tree, the app's own action registry and the app's
//! document — three things only the app has, and two of which cannot cross a
//! process boundary at all (an `ActionSpec` for a prompted action holds a
//! closure). So the wire carries the *address* out and the *instruction* back,
//! and the application performs its own operation on its own document. The
//! change then reaches everyone else the way every other change does.
//!
//! That is also what keeps the boundary honest: nothing here can write another
//! process's document, and an app that goes away takes its operations with it
//! rather than leaving the shell holding a half of them.
//!
//! ## An app may only publish its own operations
//!
//! The capability gate names the app being acted on by the namespace of the
//! resource id (`word-processor.document` → `word-processor`), because that is
//! what lets a room grant somebody edit rights in one app and not another. On
//! this path the resource id arrives from the app, so on its own it would be a
//! claim rather than a fact: an app could publish `files.entry:/…#delete` and
//! be gated as though it were Files.
//!
//! So a published operation whose namespace is not this connection's own
//! application is dropped, and said so. The connection's application is the one
//! it announced on its channel key, which the broker resolves — not the one it
//! passed in the call. After that check the namespace *is* the app, and the
//! existing gate needs no special case for a surface that came off a socket.
//!
//! ## Why an invocation is not waited for here
//!
//! The obvious shape -- send, block until the app answers -- deadlocks, and it
//! deadlocks by construction rather than by bad luck. The thread that would
//! block is the compositor's, and the compositor is what serves the
//! application's Wayland connection: an app that is mid-frame cannot reach its
//! own event loop until the compositor lets it, so the one thread waiting for
//! the answer is the one thread preventing it. Measured before this was
//! written: the operation was performed, and performed about two seconds late,
//! immediately after the wait it was waiting on gave up.
//!
//! So [`RemoteAppActions::dispatch`] sends and returns, and
//! [`RemoteAppActions::settle`] is called once per tick from the loop that
//! paints -- where clients have just been served and are able to answer. A
//! caller waiting on a socket has its reply held until then, which is the same
//! answer arriving at the only moment it can be true.
//!
//! ## Nothing here decides authority
//!
//! [`RemoteAppActions::check`] asks [`crate::shell::actions`], which asks the
//! workspace's capability model. This module holds the declarations so that an
//! operation nobody published cannot be invoked at all, and forgets them when
//! the connection ends — both halves of "fail-closed" that
//! [`ActionRegistry`] was written for.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use qos_actions::door::{ActionAddress, ActionDescriptor, InputSpec};
use qos_actions::resource::Collects;

use crate::shell::actions::{ActionDecl, ActionOutcome, ActionRegistry, check_action};
use crate::shell::collab_broker::{
    AppActionListener, BrokerHandle, PeerTable, WireExportedAction, WireInvocation,
};
use crate::shell::state::{Actor, ResourceRef, Verb, Workspace};

/// How long an invocation waits for the application to say what became of it.
///
/// Bounded, and the bound is the point. The caller asked a machine it cannot
/// see to do something, so "it ran", "it refused" and "nobody answered" must be
/// three different answers rather than one silence. A wedged application costs
/// this much and then reports honestly, which is the trade a caller can plan
/// around; an unbounded wait is one it cannot.
///
/// Two seconds because a connected app pumps its event loop at ~60 Hz
/// (`ui::App::about_to_wait` escalates to a bounded tick while a collab socket
/// is live), so an answer normally arrives inside a frame or two. Anything
/// approaching this is an app in trouble, not an app being slow.
pub const INVOCATION_TIMEOUT: Duration = Duration::from_secs(2);

/// One invocation sent and not yet answered.
struct InFlight {
    address: ActionAddress,
    /// When to stop believing an answer is coming.
    deadline: Instant,
    /// What the application said, once it has said anything.
    answer: Option<InvokeAnswer>,
}

/// One operation an application published, kept as the same inert descriptor
/// an in-process app's tree is surveyed into — so the provider serving both
/// halves has one shape to render.
#[derive(Clone, Debug)]
pub struct RemoteAction {
    /// The connection that published it, and the one an invocation goes to.
    pub peer: u64,
    /// The application, as *it* announced itself to the broker.
    pub app: String,
    pub descriptor: ActionDescriptor,
}

/// One connected application's published surface.
struct PublishedApp {
    app: String,
    /// The resource-id namespace this connection is allowed to publish under.
    namespace: String,
    actions: Vec<ActionDescriptor>,
}

#[derive(Default)]
struct Inner {
    apps: HashMap<u64, PublishedApp>,
    /// What has been declared, and so what may be asked about at all. An
    /// operation absent from here is refused by
    /// [`check_action`] as `UndeclaredAction` before any capability is
    /// consulted — the fail-closed default the registry exists for.
    decls: ActionRegistry,
    /// Invocations sent and not yet answered.
    in_flight: HashMap<u64, InFlight>,
}

/// What an application said became of an invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvokeAnswer {
    /// Empty when it ran. Otherwise one of the door's refusal codes.
    pub code: String,
    pub message: String,
}

/// Why an invocation could not be carried out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvokeError {
    /// Nothing published this address, or the connection that did has gone.
    ///
    /// A refusal by the capability model is deliberately not a variant here.
    /// This module never decides authority -- the gate runs before anything
    /// reaches [`RemoteAppActions::dispatch`], and giving a denial its own
    /// shape here would invite a caller to treat this type as the place the
    /// question is answered.
    Unknown,
    /// It was sent and the application never answered.
    TimedOut,
    /// The application answered with a refusal of its own.
    Refused { code: String, message: String },
}

impl InvokeError {
    /// The wire code, in the vocabulary `actions_invoke` already answers in.
    pub fn code(&self) -> &str {
        match self {
            // The same code an address that does not exist gets: a caller
            // must not be able to map what it may not reach by probing for a
            // different error.
            InvokeError::Unknown => "unknown_action",
            InvokeError::TimedOut => "action_failed",
            InvokeError::Refused { code, .. } => code,
        }
    }
}

impl std::fmt::Display for InvokeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InvokeError::Unknown => write!(f, "no application currently offers that operation"),
            InvokeError::TimedOut => write!(
                f,
                "the application was asked and did not answer within {}s; \
                 whether it ran is unknown",
                INVOCATION_TIMEOUT.as_secs()
            ),
            InvokeError::Refused { message, .. } => write!(f, "{message}"),
        }
    }
}

/// Everything the connected applications currently offer.
///
/// Shared rather than owned: the broker's reader threads write it, the search
/// registry reads it, and neither holds the other. `Arc<Mutex<…>>` and not the
/// `Rc<RefCell<…>>` its in-process twin uses, because the two ends are on
/// different threads.
#[derive(Clone)]
pub struct RemoteAppActions {
    inner: Arc<Mutex<Inner>>,
    /// Where an invocation leaves through. `Weak` so a broker torn down at
    /// shutdown is not kept alive by the catalogue.
    peers: Arc<Mutex<Weak<PeerTable>>>,
    next_invocation: Arc<AtomicU64>,
}

impl Default for RemoteAppActions {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteAppActions {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            peers: Arc::new(Mutex::new(Weak::new())),
            next_invocation: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Point this at the broker invocations leave through, and point the
    /// broker back here. One call, because half of either wiring is a surface
    /// that enumerates and cannot act, or one that acts and offers nothing.
    pub fn attach(self: &Arc<Self>, broker: &BrokerHandle) {
        if let Ok(mut g) = self.peers.lock() {
            *g = broker.peer_table_weak();
        }
        broker.set_app_action_listener(Arc::clone(self) as Arc<dyn AppActionListener>);
    }

    /// The resource-id namespace an application may publish under.
    ///
    /// A package id is a reverse-domain name (`dev.qos.word-processor`) and a
    /// resource id is namespaced by the app's short name
    /// (`word-processor.document`), so the last segment is the join between
    /// them. An id with no dot in it is its own namespace, which is what a
    /// dev-run app with `QUICK_APP_ID=word-processor` announces.
    pub fn namespace_of(app_id: &str) -> &str {
        app_id.rsplit('.').next().unwrap_or(app_id)
    }

    /// Every operation the connected applications currently offer.
    pub fn survey(&self) -> Vec<RemoteAction> {
        let Ok(inner) = self.inner.lock() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (peer, app) in inner.apps.iter() {
            for d in &app.actions {
                out.push(RemoteAction {
                    peer: *peer,
                    app: app.app.clone(),
                    descriptor: d.clone(),
                });
            }
        }
        // Stable order, so two enumerations of an unchanged desktop answer
        // identically. A `HashMap` iteration would not, and a caller diffing
        // one survey against the next would read the reshuffle as change.
        out.sort_by(|a, b| {
            (&a.descriptor.address.resource, &a.descriptor.address.action)
                .cmp(&(&b.descriptor.address.resource, &b.descriptor.address.action))
        });
        out
    }

    /// Which connection currently offers `address`, if any.
    fn owner_of(&self, address: &ActionAddress) -> Option<(u64, String)> {
        let inner = self.inner.lock().ok()?;
        inner.apps.iter().find_map(|(peer, app)| {
            app.actions
                .iter()
                .any(|d| d.address == *address)
                .then(|| (*peer, app.app.clone()))
        })
    }

    /// May `actor` invoke this address in `workspace`?
    ///
    /// The gate, and the only way through this module to an invocation.
    /// [`check_action`] refuses an operation nobody declared before it asks
    /// about any capability, so an address that was never published cannot be
    /// smuggled past by naming it well.
    pub fn check(
        &self,
        workspace: &Workspace,
        actor: &Actor,
        app: &str,
        action: &str,
        resource: &ResourceRef,
    ) -> ActionOutcome {
        let Ok(inner) = self.inner.lock() else {
            // A poisoned lock is a state this cannot reason about, and a gate
            // that opens when it cannot find its policy is not a gate.
            return ActionOutcome::Denied(crate::shell::actions::DenyReason::UndeclaredAction);
        };
        check_action(workspace, &inner.decls, actor, app, action, resource)
    }

    /// Ask the application that published `address` to perform it.
    ///
    /// Returns as soon as the instruction is on its way. It does **not** wait
    /// for the answer, and must not: see the module note -- the thread that
    /// would wait is the thread the application needs in order to reply.
    /// [`Self::settle`] delivers the answer from a loop that can let it run.
    ///
    /// This performs **no** permission check. The gate is a separate call
    /// ([`Self::check`]) so it cannot be skipped by reading this signature and
    /// deciding it looks complete.
    pub fn dispatch(
        &self,
        address: &ActionAddress,
        argument: Option<&str>,
        actor: &str,
    ) -> Result<u64, InvokeError> {
        let (peer, app) = self.owner_of(address).ok_or(InvokeError::Unknown)?;
        let peers = self
            .peers
            .lock()
            .ok()
            .and_then(|g| g.upgrade())
            .ok_or(InvokeError::Unknown)?;

        let id = self.next_invocation.fetch_add(1, Ordering::Relaxed);
        // Recorded *before* the send: an application quick enough to answer on
        // its next frame can answer before this returns, and a row created
        // afterwards would have nowhere to put the answer.
        {
            let mut inner = self.inner.lock().map_err(|_| InvokeError::Unknown)?;
            inner.in_flight.insert(
                id,
                InFlight {
                    address: address.clone(),
                    deadline: Instant::now() + INVOCATION_TIMEOUT,
                    answer: None,
                },
            );
        }

        let sent = peers.dispatch_invocation(
            peer,
            WireInvocation {
                id,
                resource: address.resource.clone(),
                action: address.action.clone(),
                argument: argument.map(|s| s.to_string()),
                actor: actor.to_string(),
            },
        );
        if !sent {
            if let Ok(mut inner) = self.inner.lock() {
                inner.in_flight.remove(&id);
            }
            tracing::warn!(
                peer, %app, resource = %address.resource, action = %address.action,
                "an operation was addressed to an application whose connection is not \
                 listening; nothing was asked of it"
            );
            return Err(InvokeError::Unknown);
        }
        // At `info!`, because its absence is the interesting case and a
        // level nobody films at cannot show one. The refusal beside this is
        // `warn!`, so a dispatch that never happened at all -- no refusal,
        // no ask -- looked exactly like a dispatch that worked. That is the
        // difference between a take where the picture reaches the other
        // machine and one where it does not, and it was unobservable.
        tracing::info!(
            id, peer, %app, resource = %address.resource, action = %address.action,
            "asked an application to perform one of its own operations"
        );
        Ok(id)
    }

    /// Whether anything is still waiting on an application.
    pub fn in_flight(&self) -> usize {
        self.inner.lock().map(|i| i.in_flight.len()).unwrap_or(0)
    }

    /// The invocations still out, so a caller that started some can tell which
    /// answers it is owed.
    pub fn pending_ids(&self) -> Vec<u64> {
        self.inner
            .lock()
            .map(|i| i.in_flight.keys().copied().collect())
            .unwrap_or_default()
    }

    /// Take every invocation that has been answered or has run out of time.
    ///
    /// Called once per tick from the compositor's own loop, straight after the
    /// clients have been served -- which is the first moment an application
    /// could have answered, and so the first moment this can tell "it ran"
    /// from "nobody answered".
    pub fn settle(&self) -> Vec<(u64, Result<(), InvokeError>)> {
        let Ok(mut inner) = self.inner.lock() else {
            return Vec::new();
        };
        let now = Instant::now();
        let done: Vec<u64> = inner
            .in_flight
            .iter()
            .filter(|(_, f)| f.answer.is_some() || now >= f.deadline)
            .map(|(id, _)| *id)
            .collect();
        let mut out = Vec::new();
        for id in done {
            let Some(f) = inner.in_flight.remove(&id) else {
                continue;
            };
            let result = match f.answer {
                Some(a) if a.code.is_empty() => Ok(()),
                Some(a) => Err(InvokeError::Refused {
                    code: a.code,
                    message: a.message,
                }),
                None => {
                    tracing::warn!(
                        id, resource = %f.address.resource, action = %f.address.action,
                        "an application was asked to perform one of its operations and \
                         did not answer; whether it ran is unknown"
                    );
                    Err(InvokeError::TimedOut)
                }
            };
            out.push((id, result));
        }
        out
    }
}

/// Translate one published operation into the descriptor the catalogue reads.
///
/// `None` when the wire row carries no address at all — a resource or action id
/// that is empty names nothing, and an address that resolves to nothing is
/// worse offered than withheld.
fn descriptor_of(a: WireExportedAction) -> Option<ActionDescriptor> {
    if a.resource.is_empty() || a.action.is_empty() {
        return None;
    }
    let input = match a.text_input {
        None => InputSpec::None,
        Some(t) => InputSpec::Text {
            prompt: t.prompt,
            placeholder: t.placeholder,
            collects: if t.wants_content {
                Collects::Content
            } else {
                Collects::Words
            },
        },
    };
    Some(ActionDescriptor {
        address: ActionAddress::new(a.resource, a.action),
        resource_kind: a.resource_kind,
        resource_label: a.resource_label,
        action_label: a.action_label,
        description: a.description,
        // An out-of-process app performs its own operations on its own
        // document, so from here every one of them is local to whoever runs
        // it. The app's own locus governs what it does with the action once it
        // has it, and it is the app that reads it.
        locus: qos_actions::ExecutionLocus::ClientLocal,
        input,
    })
}

impl AppActionListener for RemoteAppActions {
    fn published(&self, peer: u64, app_id: &str, room: &str, actions: Vec<WireExportedAction>) {
        let namespace = RemoteAppActions::namespace_of(app_id).to_string();
        if namespace.is_empty() {
            tracing::warn!(
                peer, %room, count = actions.len(),
                "a connection that never said which application it is published \
                 operations; none are offered, because there is nothing to gate them \
                 against"
            );
            return;
        }

        let mut kept: Vec<ActionDescriptor> = Vec::new();
        let mut foreign = 0usize;
        for a in actions {
            let Some(d) = descriptor_of(a) else { continue };
            // The namespace check. After this the resource id's namespace *is*
            // this connection's application, which is what lets the existing
            // capability gate read the app off the address without a second
            // table of who owns what.
            let claimed = crate::shell::search::providers::app_resources::app_of(
                &d.address.resource,
            );
            if claimed != namespace {
                foreign += 1;
                continue;
            }
            kept.push(d);
        }
        if foreign > 0 {
            tracing::warn!(
                peer, app = %app_id, %namespace, foreign,
                "an application published operations on resources belonging to another \
                 application; they are not offered"
            );
        }

        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        // Declared afresh: the publish is level-triggered, so an operation
        // this app has stopped offering must stop being invokable in the same
        // breath. `forget_app` then `declare` is what makes that true of the
        // registry as well as of the catalogue.
        inner.decls.forget_app(&namespace);
        for d in &kept {
            // An exported operation is by definition something that changes
            // shared state -- an app never publishes "scroll" or "select" --
            // so `Write` is what invoking one asks for. The same verb the
            // catalogue's own visibility gate assigns these, stated in both
            // places because both must agree for either to mean anything.
            inner.decls.declare(ActionDecl::new(
                namespace.clone(),
                d.address.action.clone(),
                Verb::Write,
            ));
        }
        let count = kept.len();
        inner.apps.insert(
            peer,
            PublishedApp {
                app: app_id.to_string(),
                namespace,
                actions: kept,
            },
        );
        tracing::info!(
            peer, app = %app_id, %room, count,
            "an application published what can be done to it"
        );
    }

    fn gone(&self, peer: u64) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let Some(app) = inner.apps.remove(&peer) else {
            return;
        };
        // Only when nothing else is still publishing under that namespace. Two
        // windows of one application are two connections, and the first of
        // them closing must not take the second's operations with it.
        let still_open = inner
            .apps
            .values()
            .any(|other| other.namespace == app.namespace);
        if !still_open {
            inner.decls.forget_app(&app.namespace);
        }
        tracing::info!(
            peer, app = %app.app, count = app.actions.len(), still_open,
            "an application went; what it offered is no longer addressable"
        );
    }

    fn completed(&self, id: u64, code: &str, message: &str) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        match inner.in_flight.get_mut(&id) {
            // Recorded rather than delivered: whoever is waiting is on the
            // compositor's own loop, and `settle` is where it looks.
            Some(f) => {
                f.answer = Some(InvokeAnswer {
                    code: code.to_string(),
                    message: message.to_string(),
                })
            }
            // An answer that arrived after its deadline. Not an error: the
            // caller has already been told nobody answered in time, and
            // contradicting that now would be worse than saying nothing.
            None => tracing::debug!(
                id, code, "an application answered an invocation nobody is still waiting on"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::collab_broker::WireTextInput;
    use crate::shell::state::{CapOverride, Subject, WorkspaceId};

    fn wire(resource: &str, action: &str) -> WireExportedAction {
        WireExportedAction {
            resource: resource.into(),
            action: action.into(),
            resource_kind: "word_processor.document".into(),
            resource_label: "Document".into(),
            action_label: "Insert table".into(),
            description: None,
            text_input: None,
        }
    }

    fn room() -> Workspace {
        let mut ws = Workspace::new(WorkspaceId::new(), "Study");
        ws.set_owner("quinten");
        ws
    }

    #[test]
    fn a_package_id_narrows_to_the_namespace_its_resources_use() {
        assert_eq!(
            RemoteAppActions::namespace_of("dev.qos.word-processor"),
            "word-processor"
        );
        assert_eq!(RemoteAppActions::namespace_of("dev.qos.chat"), "chat");
        // A dev-run app announces its short name directly.
        assert_eq!(RemoteAppActions::namespace_of("chat"), "chat");
    }

    /// An application may publish operations on its own resources and only
    /// those. Publishing under another app's namespace is how a sandboxed app
    /// would otherwise have its operations gated as though it were that app.
    #[test]
    fn an_app_cannot_publish_operations_in_another_apps_namespace() {
        let dir = RemoteAppActions::new();
        dir.published(
            7,
            "dev.qos.word-processor",
            "room",
            vec![
                wire("word-processor.document", "insert-table"),
                wire("files.entry:/var/workspace/plan.md", "delete"),
            ],
        );
        let offered: Vec<String> = dir
            .survey()
            .into_iter()
            .map(|a| a.descriptor.address.resource)
            .collect();
        assert_eq!(
            offered,
            vec!["word-processor.document".to_string()],
            "an operation on another application's resource must not be offered"
        );
    }

    /// The registry is rebuilt on every publish, so an operation an app has
    /// stopped offering stops being invokable rather than lingering as a
    /// declaration nothing publishes.
    #[test]
    fn republishing_a_smaller_surface_withdraws_what_it_dropped() {
        let dir = RemoteAppActions::new();
        let ws = room();
        let actor = ws.actor_for_person(&"quinten".to_string());
        let target = ResourceRef::App("word-processor".into());
        dir.published(
            1,
            "dev.qos.word-processor",
            "room",
            vec![
                wire("word-processor.document", "insert-table"),
                wire("word-processor.document", "align-left"),
            ],
        );
        assert!(
            dir.check(&ws, &actor, "word-processor", "align-left", &target)
                .is_permitted(),
            "the owner may invoke a published operation"
        );

        dir.published(
            1,
            "dev.qos.word-processor",
            "room",
            vec![wire("word-processor.document", "insert-table")],
        );
        assert_eq!(
            dir.check(&ws, &actor, "word-processor", "align-left", &target),
            ActionOutcome::Denied(crate::shell::actions::DenyReason::UndeclaredAction),
            "an operation the app has stopped publishing must stop being invokable"
        );
    }

    /// An application that goes takes its operations with it — the case
    /// `ActionRegistry::forget_app` exists for, reached from a real
    /// disconnection rather than from a test.
    #[test]
    fn an_app_that_goes_stops_offering_anything() {
        let dir = RemoteAppActions::new();
        let ws = room();
        let actor = ws.actor_for_person(&"quinten".to_string());
        let target = ResourceRef::App("word-processor".into());
        dir.published(
            4,
            "dev.qos.word-processor",
            "room",
            vec![wire("word-processor.document", "insert-table")],
        );
        assert!(
            dir.check(&ws, &actor, "word-processor", "insert-table", &target)
                .is_permitted()
        );

        dir.gone(4);
        assert!(dir.survey().is_empty(), "nothing is offered any more");
        assert_eq!(
            dir.check(&ws, &actor, "word-processor", "insert-table", &target),
            ActionOutcome::Denied(crate::shell::actions::DenyReason::UndeclaredAction),
            "an action of an app that has exited must not remain invokable"
        );
        assert!(
            dir.dispatch(
                &ActionAddress::new("word-processor.document", "insert-table"),
                None,
                "quinten",
            )
            .is_err(),
            "and the wire has nowhere to send it"
        );
    }

    /// Two windows of one application are two connections. One closing must
    /// not withdraw the other's operations.
    #[test]
    fn a_second_window_keeps_the_app_addressable() {
        let dir = RemoteAppActions::new();
        let ws = room();
        let actor = ws.actor_for_person(&"quinten".to_string());
        let target = ResourceRef::App("word-processor".into());
        for peer in [1u64, 2] {
            dir.published(
                peer,
                "dev.qos.word-processor",
                "room",
                vec![wire("word-processor.document", "insert-table")],
            );
        }
        dir.gone(1);
        assert_eq!(dir.survey().len(), 1, "the surviving window still offers it");
        assert!(
            dir.check(&ws, &actor, "word-processor", "insert-table", &target)
                .is_permitted(),
            "and it is still invokable"
        );
    }

    /// The gate is the workspace's, not this module's: a peer admitted
    /// read-only is refused an operation the owner may take.
    #[test]
    fn a_viewer_is_refused_an_operation_the_owner_may_take() {
        let dir = RemoteAppActions::new();
        let mut ws = room();
        ws.grant_capability(CapOverride::membership(
            Subject::person("xaver"),
            ws.id.raw().to_string(),
            Verb::Read,
        ));
        dir.published(
            1,
            "dev.qos.word-processor",
            "room",
            vec![wire("word-processor.document", "insert-table")],
        );
        let target = ResourceRef::App("word-processor".into());
        assert!(
            dir.check(
                &ws,
                &ws.actor_for_person(&"quinten".to_string()),
                "word-processor",
                "insert-table",
                &target
            )
            .is_permitted(),
            "the owner may"
        );
        assert!(
            !dir.check(
                &ws,
                &ws.actor_for_person(&"xaver".to_string()),
                "word-processor",
                "insert-table",
                &target
            )
            .is_permitted(),
            "a peer admitted read-only may not invoke a published operation"
        );
    }

    /// An application that takes text advertises it, and the prompt survives
    /// the crossing — a caller that cannot see the screen has nothing else to
    /// work from.
    #[test]
    fn an_operation_that_collects_content_says_so_across_the_wire() {
        let dir = RemoteAppActions::new();
        let mut row = wire("word-processor.document", "place-image");
        row.text_input = Some(WireTextInput {
            prompt: "The image to place, by content hash".into(),
            placeholder: String::new(),
            wants_content: true,
        });
        dir.published(1, "dev.qos.word-processor", "room", vec![row]);
        let surveyed = dir.survey();
        assert_eq!(surveyed.len(), 1);
        assert!(surveyed[0].descriptor.input.wants_text());
        assert!(
            surveyed[0].descriptor.input.wants_content(),
            "a hash argument must not arrive looking like free text"
        );
    }

    /// An invocation with nowhere to go is refused rather than reported as
    /// having run.
    #[test]
    fn an_invocation_with_no_broker_behind_it_is_refused() {
        let dir = RemoteAppActions::new();
        dir.published(
            1,
            "dev.qos.word-processor",
            "room",
            vec![wire("word-processor.document", "insert-table")],
        );
        let err = dir
            .dispatch(
                &ActionAddress::new("word-processor.document", "insert-table"),
                None,
                "quinten",
            )
            .expect_err("no broker is attached, so nothing can carry it");
        assert_eq!(err.code(), "unknown_action");
    }
}

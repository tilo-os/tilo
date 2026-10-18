//! Bounded, speculative local action observations. These are not authorizations.
//!
//! A collaboration route installs a scope only while processing its local input,
//! pinned to the directory revision actually published for that route. Background
//! work and delivered broker invocations must not inherit a local-input scope.
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use automerge::{AutoCommit, ChangeHash};
use tilo_actions::{ActionContext, ActionError, EncodedAction};

const CAPACITY: usize = 128;
const BYTE_BUDGET: usize = 4 * 1024 * 1024;

/// Original published invocation, before building or wrapping its typed action.
#[derive(Clone)]
pub struct PublishedInvocation {
    pub resource: String,
    pub action: String,
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

/// Exact local execution observation; an empty mutation set means read/no-op.
/// `context` retains the parent record and source invocation independently of
/// the published child address. Neither field establishes caller authority.
#[derive(Clone)]
pub struct CapturedAction {
    pub invocation: uuid::Uuid,
    /// Random identity generated once, before content binding; retained on retry.
    pub capture_id: uuid::Uuid,
    pub revision: String,
    pub published: Option<PublishedInvocation>,
    pub encoded: EncodedAction,
    pub context: Option<ActionContext>,
    pub original_heads: Vec<[u8; 32]>,
    pub mutation_hashes: Vec<[u8; 32]>,
}

impl CapturedAction {
    /// Compute the content-bound identity after the exact mutations are known.
    /// # Errors
    /// Refuses captures without an original published invocation.
    pub fn bound_invocation(&self) -> Result<uuid::Uuid, String> {
        self.clone().into_report()?.bound_invocation()
    }

    pub(crate) fn into_report(self) -> Result<runtime_collab::LocalDocumentActionReport, String> {
        let published = self
            .published
            .ok_or("local document action has no published invocation")?;
        let argument = if published.arguments.is_empty() {
            None
        } else {
            Some(format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::Value::Object(published.arguments)
            ))
        };
        Ok(runtime_collab::LocalDocumentActionReport {
            version: 1,
            invocation: self.invocation,
            capture_id: self.capture_id,
            revision: self.revision,
            resource: published.resource,
            action: published.action,
            argument,
            encoded: self.encoded,
            group: self
                .context
                .as_ref()
                .and_then(|context| context.group.clone()),
            context: self.context,
            original_heads: self.original_heads,
            mutation_hashes: self.mutation_hashes,
        })
    }
}

impl std::fmt::Debug for PublishedInvocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishedInvocation")
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for CapturedAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturedAction")
            .field("invocation", &self.invocation)
            .field("mutations", &self.mutation_hashes.len())
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct Pending {
    actions: VecDeque<(CapturedAction, Lease)>,
    reserved: usize,
    bytes: usize,
    refused: bool,
}

/// One route's UI-thread queue. Draining never performs IO, signing or blocking.
#[derive(Clone, Default)]
pub struct Queue(Rc<RefCell<Pending>>);

#[derive(Clone)]
struct Active {
    queue: Queue,
    revision: String,
    aliases: Option<Rc<std::collections::HashMap<String, String>>>,
    pin: Option<Rc<dyn std::any::Any>>,
}

/// Frozen before input: each retained component has its own route queue and
/// transport aliases from the directory snapshot that supplied `revision`.
#[derive(Clone)]
pub(crate) struct InputRoute {
    pub owner: Option<uuid::Uuid>,
    pub queue: Queue,
    pub revision: String,
    pub aliases: Rc<std::collections::HashMap<String, String>>,
    pub pin: Option<Rc<dyn std::any::Any>>,
}

thread_local! {
    static INPUT_ROUTES: RefCell<Vec<InputRoute>> = const { RefCell::new(Vec::new()) };
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
    static PUBLISHED: RefCell<Option<PublishedInvocation>> = const { RefCell::new(None) };
}

/// Nestable scope guard. Not transferable to another thread.
pub struct Scope {
    previous: Option<Active>,
    published: Option<PublishedInvocation>,
    routes: Vec<InputRoute>,
}
impl Drop for Scope {
    fn drop(&mut self) {
        ACTIVE.with(|active| *active.borrow_mut() = self.previous.take());
        PUBLISHED.with(|published| *published.borrow_mut() = self.published.take());
        INPUT_ROUTES.with(|routes| *routes.borrow_mut() = std::mem::take(&mut self.routes));
    }
}

/// Whether synchronous local input is being observed on this thread.
pub fn input_is_active() -> bool {
    ACTIVE.with_borrow(Option::is_some) || INPUT_ROUTES.with_borrow(|routes| !routes.is_empty())
}

/// Install only around a synchronous local input/semantic dispatch, never a
/// model pass. The actual route is selected at typed execution from the retained
/// component's host scope, not from whichever document happens to be primary.
pub(crate) fn enter_input(routes: Vec<InputRoute>) -> Scope {
    let scope = suspend();
    if routes.is_empty() {
        ACTIVE.with(|active| {
            *active.borrow_mut() = Some(Active {
                queue: Queue::new(),
                revision: String::new(),
                aliases: None,
                pin: None,
            })
        });
    }
    INPUT_ROUTES.with(|active| *active.borrow_mut() = routes);
    scope
}

impl Queue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enter only this route's local user input with its exact published revision.
    pub fn enter(&self, revision: String) -> Scope {
        let previous = ACTIVE.with(|active| {
            active.replace(Some(Active {
                queue: self.clone(),
                revision,
                aliases: None,
                pin: None,
            }))
        });
        let published = PUBLISHED.with(|published| published.take());
        let routes = INPUT_ROUTES.with(|routes| routes.take());
        Scope {
            previous,
            published,
            routes,
        }
    }

    pub fn drain(&self) -> Vec<CapturedAction> {
        self.drain_leased()
            .into_iter()
            .map(|(action, _)| action)
            .collect()
    }

    pub(crate) fn drain_leased(&self) -> Vec<(CapturedAction, Lease)> {
        let mut actions: Vec<_> = self.0.borrow_mut().actions.drain(..).collect();
        for (_, lease) in &mut actions {
            lease.keepalive = Some(self.clone());
        }
        actions
    }

    /// Recovered reports consume the same capacity as fresh speculative work.
    pub(crate) fn restore_lease(
        &self,
        capture: &CapturedAction,
        pin: Option<Rc<dyn std::any::Any>>,
    ) -> Result<Lease, String> {
        let heads: Vec<_> = capture
            .original_heads
            .iter()
            .copied()
            .map(ChangeHash)
            .collect();
        let bytes = report_budget(
            &capture.revision,
            capture.published.as_ref(),
            &capture.encoded,
            capture.context.as_ref(),
            &heads,
        )?;
        let mut pending = self.0.borrow_mut();
        if pending.reserved >= CAPACITY || bytes > BYTE_BUDGET.saturating_sub(pending.bytes) {
            return Err("local action recovery queue is full".into());
        }
        pending.reserved += 1;
        pending.bytes += bytes;
        Ok(Lease {
            queue: Rc::downgrade(&self.0),
            keepalive: Some(self.clone()),
            bytes,
            _pin: pin,
        })
    }

    pub(crate) fn has_outstanding(&self) -> bool {
        self.0.borrow().reserved != 0
    }

    /// Saturation is visible even when an affordance logs/swallow its error.
    pub fn take_refusals(&self) -> Vec<String> {
        if std::mem::take(&mut self.0.borrow_mut().refused) {
            vec!["local action observation could not be retained".into()]
        } else {
            Vec::new()
        }
    }
}

/// Exclude native work or a retained remote continuation from local-user capture.
/// The surrounding route scope is restored on drop.
pub fn suspend() -> Scope {
    let previous = ACTIVE.with(|active| active.take());
    let published = PUBLISHED.with(|published| published.take());
    let routes = INPUT_ROUTES.with(|routes| routes.take());
    Scope {
        previous,
        published,
        routes,
    }
}

/// Temporarily retain an invocation before the framework builds its action.
/// An inner typed apply keeps the outer named invocation instead of replacing it.
pub fn with_invocation(invocation: PublishedInvocation) -> InvocationScope {
    let previous = PUBLISHED.with(|published| published.replace(Some(invocation)));
    InvocationScope { previous }
}

pub struct InvocationScope {
    previous: Option<PublishedInvocation>,
}
impl Drop for InvocationScope {
    fn drop(&mut self) {
        PUBLISHED.with(|published| *published.borrow_mut() = self.previous.take());
    }
}

pub fn has_invocation() -> bool {
    PUBLISHED.with(|published| published.borrow().is_some())
}

/// Reserve before mutation: saturation refuses without speculative writes.
pub(crate) fn reserve(
    encoded: &EncodedAction,
    context: Option<&ActionContext>,
    doc: &mut AutoCommit,
    before: &[ChangeHash],
) -> Result<Option<Reservation>, ActionError> {
    // A continuation retains its original broker owner; a user-input scope
    // must never relabel it as another fresh local invocation.
    if context.is_some_and(|context| {
        context.source_invocation.is_some()
            || matches!(context.change, tilo_actions::ChangeKind::External)
    }) {
        return Ok(None);
    }
    let active = ACTIVE.with(|active| active.borrow().clone()).or_else(|| {
        let owner = crate::bound_dispatch::executing_host_scope();
        INPUT_ROUTES.with_borrow(|routes| {
            routes
                .iter()
                .find(|route| route.owner == owner)
                .map(|route| Active {
                    queue: route.queue.clone(),
                    revision: route.revision.clone(),
                    aliases: Some(route.aliases.clone()),
                    pin: route.pin.clone(),
                })
        })
    });
    let Some(active) = active else {
        if input_is_active() {
            INPUT_ROUTES.with_borrow(|routes| {
                for route in routes {
                    route.queue.0.borrow_mut().refused = true;
                }
            });
            return Err(ActionError::Refused(
                "local action has no retained input route".into(),
            ));
        }
        return Ok(None);
    };
    let mut published = PUBLISHED.with(|published| published.borrow().clone());
    if published.is_none() {
        // Compatibility actions have no exact semantic address to attest.
        // Preserve their local behavior, but make the observation gap visible;
        // never invent an action from an encoded type or a display label.
        active.queue.0.borrow_mut().refused = true;
        return Ok(None);
    }
    if let (Some(invocation), Some(aliases)) = (&mut published, &active.aliases) {
        let Some(alias) = aliases.get(&invocation.resource) else {
            active.queue.0.borrow_mut().refused = true;
            return Err(ActionError::Refused(
                "local action is absent from its published resource snapshot".into(),
            ));
        };
        invocation.resource.clone_from(alias);
    }
    let estimate = report_budget(
        &active.revision,
        published.as_ref(),
        encoded,
        context,
        before,
    );
    let bytes = match estimate {
        Ok(bytes) => bytes,
        Err(error) => {
            active.queue.0.borrow_mut().refused = true;
            return Err(ActionError::Refused(error));
        }
    };
    let mut pending = active.queue.0.borrow_mut();
    if active.revision.is_empty()
        || pending.reserved >= CAPACITY
        || bytes > BYTE_BUDGET.saturating_sub(pending.bytes)
    {
        pending.refused = true;
        return Err(ActionError::Refused(
            "local action observation queue is full".into(),
        ));
    }
    pending.reserved += 1;
    pending.bytes += bytes;
    drop(pending);
    Ok(Some(Reservation {
        active,
        published,
        bytes,
        actor: doc.get_actor().clone(),
    }))
}

pub(crate) struct Reservation {
    active: Active,
    published: Option<PublishedInvocation>,
    bytes: usize,
    actor: automerge::ActorId,
}
impl Reservation {
    pub(crate) fn finish(
        mut self,
        doc: &mut AutoCommit,
        before: &[ChangeHash],
        encoded: EncodedAction,
        context: Option<ActionContext>,
    ) -> Result<(), ActionError> {
        // Flush the executor's committed changes, never substitute only the tips.
        doc.get_heads();
        if before.len() > 256 || doc.get_changes(before).len() > 256 {
            self.active.queue.0.borrow_mut().refused = true;
            // Actions may explicitly commit multiple changes. Do not leave an
            // unreportable speculative document behind if that exceeds the
            // conservative bound reserved before execution.
            let mut restored = doc.fork_at(before)?;
            restored.set_actor(self.actor.clone());
            *doc = restored;
            return Err(ActionError::Refused(
                "local action exceeds hash budget".into(),
            ));
        }
        let mut original_heads = before.iter().map(|hash| hash.0).collect::<Vec<_>>();
        original_heads.sort_unstable();
        original_heads.dedup();
        let mut mutation_hashes = doc
            .get_changes(before)
            .iter()
            .map(|change| change.hash().0)
            .collect::<Vec<_>>();
        mutation_hashes.sort_unstable();
        mutation_hashes.dedup();
        let mut action = CapturedAction {
            invocation: uuid::Uuid::nil(),
            capture_id: uuid::Uuid::new_v4(),
            revision: self.active.revision.clone(),
            published: self.published.clone(),
            encoded,
            context,
            original_heads,
            mutation_hashes,
        };
        // Only the finalized mutation set can define the immutable invocation.
        action.invocation = action.bound_invocation().map_err(ActionError::Refused)?;
        self.active.queue.0.borrow_mut().actions.push_back((
            action,
            Lease {
                queue: Rc::downgrade(&self.active.queue.0),
                keepalive: None,
                bytes: self.bytes,
                _pin: self.active.pin.clone(),
            },
        ));
        // The outstanding lease survives draining and transport submission.
        self.bytes = 0;
        Ok(())
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut pending = self.active.queue.0.borrow_mut();
        if self.bytes != 0 {
            pending.reserved -= 1;
            pending.bytes -= self.bytes;
        }
    }
}

/// Capacity and snapshot ownership remain outstanding until a definitive reply.
pub(crate) struct Lease {
    queue: std::rc::Weak<RefCell<Pending>>,
    keepalive: Option<Queue>,
    bytes: usize,
    _pin: Option<Rc<dyn std::any::Any>>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(queue) = self.queue.upgrade() {
            let mut pending = queue.borrow_mut();
            pending.reserved -= 1;
            pending.bytes -= self.bytes;
        }
    }
}

// Borrowed wire shape of LocalDocumentActionReport. Tests compare its count to
// the transport encoder so changes to that envelope cannot silently underbudget.
#[derive(serde::Serialize)]
struct BudgetReport<'a> {
    version: u8,
    invocation: uuid::Uuid,
    capture_id: uuid::Uuid,
    revision: &'a str,
    resource: &'a str,
    action: &'a str,
    argument: Option<ReportArgument<'a>>,
    encoded: &'a EncodedAction,
    context: Option<&'a ActionContext>,
    original_heads: &'a [[u8; 32]],
    mutation_hashes: &'a [[u8; 32]],
    group: Option<&'a tilo_actions::ActionGroup>,
}

struct ReportArgument<'a>(&'a serde_json::Map<String, serde_json::Value>);

impl serde::Serialize for ReportArgument<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl std::fmt::Display for ReportArgument<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(tilo_actions::ARGUMENTS_JSON_PREFIX)?;
        serde_json::to_writer(ArgumentFormatter(formatter), self.0).map_err(|_| std::fmt::Error)
    }
}

// Stream the inner JSON through the outer JSON string escaper, rather than
// allocating a cloned argument tree, an argument string and a report buffer.
struct ArgumentFormatter<'a, 'b>(&'a mut std::fmt::Formatter<'b>);

impl std::io::Write for ArgumentFormatter<'_, '_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let text = std::str::from_utf8(bytes).map_err(std::io::Error::other)?;
        self.0.write_str(text).map_err(std::io::Error::other)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct ReportCounter {
    bytes: usize,
}

impl ReportCounter {
    fn add(&mut self, bytes: usize) -> std::io::Result<()> {
        if bytes > (64 * 1024_usize).saturating_sub(self.bytes) {
            return Err(std::io::Error::other(
                "local action exceeds report wire budget",
            ));
        }
        self.bytes += bytes;
        Ok(())
    }
}

impl std::io::Write for ReportCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.add(bytes.len())?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn validate_report_field(field: &str) -> Result<(), String> {
    if field.is_empty() || field.len() > 4096 || field.chars().any(char::is_control) {
        return Err("invalid local document report field".into());
    }
    Ok(())
}

fn report_budget(
    revision: &str,
    published: Option<&PublishedInvocation>,
    encoded: &EncodedAction,
    context: Option<&ActionContext>,
    before: &[ChangeHash],
) -> Result<usize, String> {
    let published = published.ok_or("local action has no published invocation")?;
    if before.len() > 256 {
        return Err("local action exceeds head budget".into());
    }
    for field in [
        revision,
        published.resource.as_str(),
        published.action.as_str(),
    ] {
        validate_report_field(field)?;
    }
    validate_report_field(&encoded.type_id)?;
    // Validate without constructing a second JSON tree for the typed payload.
    serde_json::from_slice::<serde::de::IgnoredAny>(&encoded.payload)
        .map_err(|error| error.to_string())?;
    let group = context.and_then(|cx| cx.group.as_ref());
    if let Some(group) = group {
        for field in [&group.id, &group.label, &group.kind] {
            validate_report_field(field)?;
        }
    }
    let mut counter = ReportCounter::default();
    serde_json::to_writer(
        &mut counter,
        &BudgetReport {
            version: 1,
            // UUIDs have fixed wire width; budgeting must not request randomness.
            invocation: uuid::Uuid::from_u128(1),
            capture_id: uuid::Uuid::from_u128(1),
            revision,
            resource: &published.resource,
            action: &published.action,
            argument: (!published.arguments.is_empty())
                .then_some(ReportArgument(&published.arguments)),
            encoded,
            context,
            original_heads: &[],
            mutation_hashes: &[],
            group,
        },
    )
    .map_err(|error| error.to_string())?;
    // A worst-case decimal hash occupies 129 bytes, plus its separator.
    // Replace the empty arrays with bounded hash lists, without allocating them.
    let hashes = [before.len(), 256]
        .into_iter()
        .filter(|count| *count != 0)
        .map(|count| count * 130 - 1)
        .sum::<usize>();
    counter.add(hashes).map_err(|error| error.to_string())?;
    Ok(counter.bytes)
}

#[cfg(test)]
#[path = "local_document_action_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "local_document_input_tests.rs"]
mod input_tests;

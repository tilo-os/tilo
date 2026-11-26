//! Host-local invocation ownership. No wire caller can manufacture these checks.

#[cfg(feature = "broker-transport")]
pub use ::collaboration::invocation_scope::ExternalModerationReply;
pub use ::collaboration::invocation_scope::{
    InvokerScope, current_invoker, current_presence, delivered_approval, delivered_invocation,
};

pub use crate::widget::resource_operation::ExternalModerationFuture;

thread_local! {
    static VIEWERS: std::cell::RefCell<Vec<tilo_actions::PresenceId>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The presence a published tree is being built for while it lives: what a surface offers bound to
/// where someone is (their caret, their Find hits) is that presence's, not the window's.
pub struct ViewerScope(());
impl ViewerScope {
    pub fn enter(presence: tilo_actions::PresenceId) -> Self {
        VIEWERS.with_borrow_mut(|viewers| viewers.push(presence));
        Self(())
    }
}
impl Drop for ViewerScope {
    fn drop(&mut self) {
        VIEWERS.with_borrow_mut(|viewers| {
            viewers.pop();
        });
    }
}

/// Whose view what is being built or resolved now is: the presence of the delivered invocation, else
/// the presence a tree is published for.
pub fn current_viewer() -> Option<tilo_actions::PresenceId> {
    current_presence().or_else(|| VIEWERS.with_borrow(|viewers| viewers.last().cloned()))
}

/// Who an effect invocation runs for, kept while it runs: what its handler and completion submit is
/// applied for the same invoker, in the same context, and only as far as the invoker was admitted.
#[derive(Clone, Debug)]
pub(crate) struct Continuation {
    pub(crate) invoker: tilo_actions::ActionInvoker,
    pub(crate) context: Option<tilo_actions::ActionContext>,
    /// What the invoker was admitted for: the effect's own kind, which the admitting host checked the
    /// invoker may perform.
    pub(crate) admitted: tilo_actions::ActionKind,
    /// Why an edit the effect submitted was refused: the effect then fails.
    pub(crate) refused: Option<tilo_actions::ActionFailure>,
    /// The edits it submitted that the host is still admitting, by request.
    pub(crate) outstanding: std::collections::HashSet<u64>,
    /// The effect's answer, held until those edits are admitted or refused: the wire id to send it
    /// under, and the answer.
    pub(crate) deferred: Option<(u64, tilo_actions::InvocationUpdate)>,
}

impl Continuation {
    /// Enter the invoker's scope for one edit the effect submitted.
    pub(crate) fn enter(&self) -> (InvokerScope, ContinuationScope) {
        (
            InvokerScope::enter(self.invoker.clone()),
            ContinuationScope::enter(self.admitted),
        )
    }

    /// Why the effect fails when the edit it submitted was refused with `error`: a refusal keeps its
    /// own code.
    pub(crate) fn failure(error: &tilo_actions::ApiError) -> tilo_actions::ActionFailure {
        let (code, message) = match error {
            tilo_actions::ApiError::Unavailable { code, reason } => (code.clone(), reason.clone()),
            error => (
                crate::bound_dispatch::api_error_code(error).to_owned(),
                error.to_string(),
            ),
        };
        tilo_actions::ActionFailure {
            code,
            message,
            details: error.failure_details(),
            retryable: false,
        }
    }

    /// The effect's terminal `update`, failed when an edit it submitted was refused.
    pub(crate) fn settle(
        &self,
        update: tilo_actions::InvocationUpdate,
    ) -> tilo_actions::InvocationUpdate {
        match (update, &self.refused) {
            (tilo_actions::InvocationUpdate::Succeeded { invocation, .. }, Some(error)) => {
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: error.clone(),
                }
            }
            (update, _) => update,
        }
    }
}

thread_local! {
    static CONTINUATION_BOUNDS: std::cell::RefCell<Vec<tilo_actions::ActionKind>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// An edit an effect submitted on its invoker's behalf is being applied on this thread: it may need no
/// more than the invoker was admitted for.
#[must_use = "the bound applies only while the guard is alive"]
pub(crate) struct ContinuationScope(());
impl ContinuationScope {
    pub(crate) fn enter(admitted: tilo_actions::ActionKind) -> Self {
        CONTINUATION_BOUNDS.with_borrow_mut(|bounds| bounds.push(admitted));
        Self(())
    }
}
impl Drop for ContinuationScope {
    fn drop(&mut self) {
        CONTINUATION_BOUNDS.with_borrow_mut(|bounds| {
            bounds.pop();
        });
    }
}

/// Refuse an edit of kind `needed` while applying an effect's edit for an invoker admitted for less (a
/// read never lends a change, a change never an outside effect): an effect never lends its invoker more
/// than they hold.
pub(crate) fn check_continuation(
    spec: &tilo_actions::ActionSpec,
    needed: tilo_actions::ActionKind,
) -> Result<(), tilo_actions::ApiError> {
    let admitted = CONTINUATION_BOUNDS.with_borrow(|bounds| bounds.last().copied());
    match admitted {
        Some(admitted) if admitted < needed => Err(tilo_actions::ApiError::Unavailable {
            code: "permission_denied".into(),
            reason: format!("the invoker may not {} here", spec.label.to_lowercase()),
        }),
        _ => Ok(()),
    }
}

/// Whether `actor` may invoke the View operation `contract` here. The local person always may.
pub fn view_operation_permitted(
    actor: &tilo_actions::Actor,
    local_author: &tilo_actions::MemberId,
    contract: &tilo_actions::ActionContract,
) -> bool {
    actor.member_id() == Some(local_author)
        || contract.reads_resource()
        || (contract.is_presence_view()
            && current_invoker()
                .is_some_and(|invoker| invoker.acting.by == *actor && invoker.presence.is_some()))
}

/// Refuse `spec` on `resource` for `actor` when only the person at this
/// device invokes it and `actor` is someone else: a view of their window
/// ([`view_operation_permitted`]), an effect that only changes this screen
/// ([`tilo_actions::ActionContract::only_shows`]), or anything on a
/// control the window draws ([`tilo_actions::kind_is_a_drawn_control`]): a
/// press, drag or focus stands in for this person's pointer and keyboard.
/// An agent or another member never moves this person's screen. An effect
/// that shows a row is answered against the caller's own view
/// ([`tilo_actions::SCREEN_PLACE_TAG`]).
pub(crate) fn check_shown_here(
    actor: &tilo_actions::Actor,
    local_author: &tilo_actions::MemberId,
    resource: &tilo_actions::ResourceSpec,
    spec: &tilo_actions::ActionSpec,
) -> Result<(), tilo_actions::ApiError> {
    let someone_else = actor.member_id() != Some(local_author);
    let private = match spec.kind {
        _ if tilo_actions::kind_is_a_drawn_control(&resource.kind) => someone_else,
        tilo_actions::OperationKind::View => {
            !view_operation_permitted(actor, local_author, &spec.contract)
        }
        tilo_actions::OperationKind::Effect => spec.contract.only_shows() && someone_else,
        tilo_actions::OperationKind::Document => false,
    };
    if private {
        return Err(tilo_actions::ApiError::Unavailable {
            code: tilo_actions::Availability::PRIVATE_VIEW.into(),
            reason: format!("{} {}", spec.label, tilo_actions::Availability::SHOWN_HERE),
        });
    }
    Ok(())
}

use crate::collaboration::RouteSpec;
use crate::resource::{DocumentCheckout, ResourceStore, resource_loan_matches};
use automerge::AutoCommit;
use std::cell::RefCell;

/// The record a disclosed read names: its workspace when it is shared in one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResourceStamp {
    pub(crate) workspace: Option<uuid::Uuid>,
    pub(crate) app_id: String,
    pub(crate) document_id: String,
}

impl From<&RouteSpec> for ResourceStamp {
    fn from(route: &RouteSpec) -> Self {
        Self {
            workspace: Some(route.cluster_id),
            app_id: route.app_id.clone(),
            document_id: route.document.clone().unwrap_or_default(),
        }
    }
}

struct MountedDocument {
    stamp: ResourceStamp,
    document: *const AutoCommit,
}
thread_local! {
    static MOUNTED_DOCUMENTS: RefCell<Vec<MountedDocument>> = const { RefCell::new(Vec::new()) };
}
struct MountedLoan;
impl Drop for MountedLoan {
    fn drop(&mut self) {
        MOUNTED_DOCUMENTS.with_borrow_mut(|mounted| {
            mounted.pop();
        });
    }
}

/// Lend `document` to `callback`, identified as the record `stamp` names.
fn with_identified_document<R>(
    stamp: Option<ResourceStamp>,
    document: &mut AutoCommit,
    callback: impl FnOnce(&mut AutoCommit) -> R,
) -> R {
    let Some(stamp) = stamp else {
        return callback(document);
    };
    MOUNTED_DOCUMENTS.with_borrow_mut(|mounted| {
        mounted.push(MountedDocument { stamp, document });
    });
    let _loan = MountedLoan;
    callback(document)
}

/// Lend the app's mounted primary document to `callback`, identified as the canonical record on
/// `route`.
pub(crate) fn with_mounted_document<R>(
    route: Option<RouteSpec>,
    document: &mut AutoCommit,
    callback: impl FnOnce(&mut AutoCommit) -> R,
) -> R {
    with_identified_document(route.as_ref().map(ResourceStamp::from), document, callback)
}

/// Lend a component's record document to `callback`, identified only when it is the record store's
/// current loan of exactly `checkout`'s document: on its canonical `route` when it has one, else as
/// `app_id`'s local record.
pub(crate) fn with_loaned_resource<R>(
    store: &ResourceStore,
    checkout: &DocumentCheckout,
    app_id: &str,
    route: Option<RouteSpec>,
    document: &mut AutoCommit,
    callback: impl FnOnce(&mut AutoCommit) -> R,
) -> R {
    let stamp = resource_loan_matches(store, checkout, document).then(|| {
        route.as_ref().map_or_else(
            || ResourceStamp {
                workspace: None,
                app_id: app_id.to_string(),
                document_id: checkout.id.as_str().to_string(),
            },
            ResourceStamp::from,
        )
    });
    with_identified_document(stamp, document, callback)
}

/// The record the innermost identified document names, when it is exactly `document`.
pub(crate) fn identified_resource(document: &AutoCommit) -> Option<ResourceStamp> {
    MOUNTED_DOCUMENTS.with_borrow(|mounted| {
        mounted
            .last()
            .filter(|mount| std::ptr::eq(mount.document, document))
            .map(|mount| mount.stamp.clone())
    })
}

//! A tilolang app's record, read and changed through the package the host runs it from.
//!
//! A record whose app is built into the host's own build (`so.tilo.chat`, its Notes and Tasks)
//! is no catalogue app and no installed `.tapp`: its resources live in the workspace's resource
//! store, and the host that keeps them reads and writes them through the app's package
//! ([`WorkspaceDocumentsLink::runs_package_record`]). This lists what it publishes, and applies
//! its published actions as the caller: a read answers the core read as the person the caller
//! acts for reads it, and a write is written as theirs once their grants allowed it here
//! ([`WorkspaceDocumentsLink::apply_package_record`]).
//!
//! What a caller may read and change is decided here, where every other caller's is: their
//! access in the workspace, narrowed by the rules an agent's turn carries.

use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{Caller, CatalogueRow, InvokeRequest, RemoteActionTarget};
use tilo_actions::{ActionDescriptor, ActionInvoker, ActionOutput};
use tilo_server_protocol::WorkspaceId;

use super::{
    ActionCallError, Answer, CatalogueQuery, DocumentRoute, Outcome, PackageRecordCall, TreePath,
    Worker, bounded, carried, confirms, direct, run_context, unknown_action,
};

impl Worker {
    /// Whether `route` is a record whose app this host runs from its own package.
    pub(super) fn package_record(&self, workspace_id: WorkspaceId, route: &DocumentRoute) -> bool {
        self.shared
            .link
            .get()
            .is_some_and(|link| link.runs_package_record(workspace_id, route))
    }

    /// Everything the record at `route` publishes, in the rows a catalogue lists, read through
    /// the app's package. `None` for a record this host runs no package of, or one it cannot read
    /// now.
    fn published_rows(
        &self,
        workspace_id: WorkspaceId,
        route: &DocumentRoute,
    ) -> Option<Vec<CatalogueRow>> {
        self.shared
            .link
            .get()?
            .package_record_rows(workspace_id, route, self.member.as_str())
    }

    /// What the record at `route` publishes, as `bounded`'s caller may read and change it: each
    /// row the caller's own access denies is left out, as a catalogue app's rows are. `None` for
    /// a record this host runs no package of.
    pub(super) fn package_rows(
        &self,
        workspace_id: WorkspaceId,
        bounded: Option<&bounded::Bounded>,
        route: &DocumentRoute,
    ) -> Option<Result<Vec<CatalogueRow>, String>> {
        if !self.package_record(workspace_id, route) {
            return None;
        }
        let rows = self.published_rows(workspace_id, route)?;
        let permissions = match bounded {
            Some(who) => match self.permissions_at(workspace_id, who, route) {
                Some(permissions) => Some(permissions),
                None => {
                    return Some(Err(format!(
                        "{} is not admitted to this resource",
                        route.app_id
                    )));
                }
            },
            None => None,
        };
        Some(Ok(rows
            .into_iter()
            .filter(|row| {
                permissions.as_ref().is_none_or(|permissions| {
                    row.action.as_ref().is_none_or(|action| {
                        permissions.answer(&TreePath::root(), action.contract.kind) != Answer::Deny
                    })
                })
            })
            .collect()))
    }

    /// One page of [`Self::package_rows`]: the rows `query` admits in `window` (an offset and
    /// how many), and how many it admits in all. `None` for a record this host runs no package
    /// of.
    pub(super) fn package_page(
        &self,
        workspace_id: WorkspaceId,
        bounded: Option<&bounded::Bounded>,
        route: &DocumentRoute,
        query: &CatalogueQuery,
        (skip, take): (usize, usize),
    ) -> Option<Result<(usize, Vec<CatalogueRow>), String>> {
        let rows = self.package_rows(workspace_id, bounded, route)?;
        Some(rows.map(|rows| page(rows, query, (skip, take))))
    }

    /// The action `address` names among what the record at `route` publishes: the verb of that
    /// name, or the only resource declaring it when the address names none.
    pub(super) fn package_descriptor(
        &self,
        workspace_id: WorkspaceId,
        route: &DocumentRoute,
        address: &ActionAddress,
    ) -> Option<ActionDescriptor> {
        let mut declaring = self
            .published_rows(workspace_id, route)?
            .into_iter()
            .filter_map(|row| row.action)
            .filter(|action| {
                (address.resource.is_empty() || action.address.resource == address.resource)
                    && action.address.action == address.action
            });
        let first = declaring.next()?;
        if address.resource.is_empty() && declaring.next().is_some() {
            return None;
        }
        Some(first)
    }

    /// Apply `request`, an action of the record at `route`, as `caller`: a read answers what the
    /// core read answers the person they act for, and a write lands as that member's write, which
    /// the workspace admits under their grants. `target` names the action, `arguments` are its
    /// own, and `withheld` keeps from the caller what is said in a direct conversation their
    /// person takes no part in.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt answered"
    )]
    pub(super) fn perform_package_record(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        route: &DocumentRoute,
        (target, arguments, withheld): (
            &RemoteActionTarget,
            serde_json::Map<String, serde_json::Value>,
            &direct::Kept,
        ),
    ) -> Result<(ActionOutput, Option<super::ActorPlace>), Outcome> {
        let Some(link) = self.shared.link.get().cloned() else {
            return Err(unknown_action());
        };
        let Some(descriptor) = self.package_descriptor(workspace_id, route, &target.address) else {
            return Err(unknown_action());
        };
        withheld.refuses(&target.address.resource)?;
        // The person's answer to this very invocation, from a device of theirs, or a lasting
        // permission their own device gave for it.
        let approval = self
            .log()
            .entries
            .get(&request.invocation)
            .and_then(|entry| entry.approval.clone())
            .map(super::Confirmation::Approved);
        let given = approval.as_ref().or(request.confirmation.as_ref());
        let binding = bounded::ApprovalBinding::of(workspace_id, caller, request);
        let confirmed = confirms(&descriptor.contract.confirmation, given, |token| {
            self.shared.approvals.take(token, &binding)
        });
        // What the caller's own access leaves them: an action their grants refuse is refused
        // here, one they leave to the person waits for their answer, and everything else is
        // decided by the workspace when the write lands, as a bot's write is.
        let who = self.bounded(caller);
        let person = who
            .as_ref()
            .map_or_else(|| self.member.clone(), |who| who.person.clone());
        if caller.roster_created_at().is_some() {
            if !caller.execution_is_live() || caller.turn().is_none() {
                return Err(Outcome::Rejected(ActionCallError::NotPermitted));
            }
            if who
                .as_ref()
                .is_some_and(|who| self.permissions_at(workspace_id, who, route).is_none())
            {
                return Err(Outcome::Rejected(ActionCallError::NotPermitted));
            }
        } else if !self.serves_as_owner(caller) {
            let who = who.ok_or_else(unknown_action)?;
            if !self.admits_bounded(workspace_id, &who) {
                return Err(Outcome::Rejected(ActionCallError::NotPermitted));
            }
            let Some(permissions) = self.permissions_at(workspace_id, &who, route) else {
                return Err(Outcome::Rejected(ActionCallError::NotPermitted));
            };
            match permissions.answer(&TreePath::root(), descriptor.contract.kind) {
                Answer::Allow => {}
                Answer::Deny => return Err(Outcome::Rejected(ActionCallError::NotPermitted)),
                Answer::Ask if confirmed => {}
                Answer::Ask => {
                    let about = self.about(
                        workspace_id,
                        route,
                        &descriptor,
                        arguments.clone(),
                        carried::Carries::Nothing,
                    );
                    return Err(self.waits_for_person(request.invocation, about));
                }
            }
        } else if descriptor.contract.requires_confirmation() && !confirmed {
            return Err(Outcome::Rejected(ActionCallError::confirmation_required()));
        }
        let context = run_context::caller_context(caller, request);
        let invoker =
            ActionInvoker::admitted(caller.acting()).named(caller.agent_name().map(str::to_owned));
        let arguments = withheld.given(descriptor.bind_prompt_text(arguments))?;
        let answer = link
            .apply_package_record(
                workspace_id,
                PackageRecordCall {
                    app: &route.app_id,
                    document: &route.document,
                    person: person.as_str(),
                    address: &descriptor.address,
                    arguments,
                    invoker: &invoker,
                    context: &context,
                    read: descriptor.contract.kind == super::ActionKind::Read,
                },
            )
            .ok_or_else(unknown_action)?;
        match answer {
            Ok(output) => {
                if descriptor.contract.kind == super::ActionKind::Read {
                    self.note_reads(request);
                }
                Ok((output, None))
            }
            Err(error) => Err(super::api_outcome(&error)),
        }
    }
}

/// `rows` as one page of a listing: the rows `query` admits, skipping `skip` and keeping `take`.
fn page(
    rows: Vec<CatalogueRow>,
    query: &CatalogueQuery,
    (skip, take): (usize, usize),
) -> (usize, Vec<CatalogueRow>) {
    let mut total = 0_usize;
    let mut page = Vec::new();
    for row in rows.into_iter().filter(|row| query.admits(row)) {
        if total >= skip && page.len() < take {
            page.push(row);
        }
        total = total.saturating_add(1);
    }
    (total, page)
}

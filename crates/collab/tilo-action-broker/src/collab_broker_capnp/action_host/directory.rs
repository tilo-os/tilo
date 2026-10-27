//! Providers that publish a live action directory instead of an eager
//! surface: browsing a page of it, resolving a canonical route's provider,
//! and admitting an action its owner re-resolves on the broker's local set.

use std::sync::Arc;

use tilo_actions::remote::{
    ActionHostError, CONFIRMED_ACTION_NEEDS_REVISION, Caller, CataloguePage, CatalogueProvider,
    CatalogueQuery, CatalogueResource, CatalogueRow, InvocationReceipt, InvokeRequest, Pending,
    Reservation, pending,
};

use super::super::{
    ActionDirectoryBridgeError, ActionDirectoryBridgeReply, ActionDirectoryBridgeResponse,
    ActionDirectoryRoot, CapnpBrokerState, InvocationInputs, PublishedSurface,
    published_directory_action, validate_directory_target_input,
};
use super::{Started, check_execution, descriptor, oversized_argument, refusal};
use crate::collab_broker::CrdtRoute;

impl CapnpBrokerState {
    /// The directory root publishing `peer_id` in a workspace `caller` may
    /// act in, with the actor the caller is admitted as there.
    pub(super) fn directory_root_for(
        &self,
        caller: &Caller,
        peer_id: u64,
    ) -> Option<ActionDirectoryRoot> {
        let rooms = self.rooms_for(caller);
        self.action_directory_roots()
            .into_iter()
            .find(|root| root.peer_id == peer_id && rooms.contains(&root.cluster_id))
    }

    pub(super) fn canonical_provider_page(
        &self,
        caller: &Caller,
        query: &CatalogueQuery,
    ) -> Result<CataloguePage, ActionHostError> {
        let route = query
            .canonical_route
            .as_ref()
            .ok_or(ActionHostError::UnknownAction)?;
        if query.max_results != 0
            || query.provider.is_some()
            || query.app_id.is_some()
            || query.view_resource.is_some()
            || query.parent_resource.is_some()
            || !query.needle.is_empty()
            || query.target.is_some()
            || query.revision.is_some()
            || !query.cursor.is_empty()
            || query.limits.is_some()
        {
            return Err(ActionHostError::InvalidRequest {
                reason: "a canonical route lookup must request only its provider".into(),
            });
        }
        if self.decide_for(caller, &route.room, |_, _, _| ()).is_none() {
            return Err(ActionHostError::NotPermitted);
        }
        let mut page = CataloguePage {
            exhausted: true,
            revision: "canonical-route-v1".into(),
            ..CataloguePage::default()
        };
        if let Some((root, token)) = self.canonical_directory(route)? {
            page.providers.push(CatalogueProvider {
                provider: token,
                app_id: root.app_id,
                room: root.cluster_id,
                record_id: root.record_id,
                browsable: true,
            });
        }
        Ok(page)
    }

    pub(super) fn query_directory(
        &self,
        caller: &Caller,
        root: &ActionDirectoryRoot,
        query: &CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        // A directory root publishes one application's rows, so a query for
        // another application is answered empty without asking it: nothing
        // it holds could match. The caller's room was checked before here.
        if query
            .app_id
            .as_deref()
            .is_some_and(|app| app != root.app_id.as_str())
        {
            return Pending::ready(Ok(CataloguePage {
                exhausted: true,
                ..CataloguePage::default()
            }));
        }
        let actor = self.caller_actor(caller, &root.cluster_id);
        let queue = self.action_directory_request_queue();
        let reply = if let Some(target) = &query.target {
            let Some(revision) = query
                .revision
                .clone()
                .filter(|revision| !revision.is_empty())
            else {
                return Pending::ready(Err(ActionHostError::InvalidRequest {
                    reason: "resolving a target needs the revision it was chosen at".into(),
                }));
            };
            queue.try_resolve_pending(
                actor,
                &root.cluster_id,
                root.peer_id,
                revision,
                &target.resource,
                &target.action,
            )
        } else {
            let limits = query.limits.unwrap_or_default();
            queue.try_query_pending(
                actor,
                &root.cluster_id,
                runtime_collab::ActionDirectoryQuery {
                    peer_id: root.peer_id,
                    revision: query.revision.clone().unwrap_or_default(),
                    cursor: query.cursor.clone(),
                    needle: query.needle.clone(),
                    parent_resource: query.parent_resource.clone().unwrap_or_default(),
                    max_results: u32::try_from(query.page_size()).unwrap_or(1),
                    // The broker clamps every budget to its own maxima.
                    max_bytes: limits.max_bytes.unwrap_or(u32::MAX),
                    max_work: limits.max_work.unwrap_or(u32::MAX),
                    max_depth: limits.max_depth.unwrap_or(u16::MAX),
                    resource_name: query.resource_name.clone().unwrap_or_default(),
                },
            )
        };
        let root = root.clone();
        let caller = caller.clone();
        match reply {
            Ok(reply) => reply.map(move |reply| {
                check_execution(&caller)?;
                directory_page(&root, reply)
            }),
            Err(error) => Pending::ready(Err(bridge_error(error))),
        }
    }

    /// The directory publishing `request`'s target, when the action is not
    /// on the provider's eager surface.
    pub(super) fn directory_target(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<ActionDirectoryRoot> {
        let peer_id = request.target.provider.parse::<u64>().ok()?;
        let address = &request.target.address;
        let eager = self.surfaces.lock().is_ok_and(|surfaces| {
            surfaces.get(&peer_id).is_some_and(|surface| {
                surface.live
                    && surface.actions.iter().any(|action| {
                        action.resource == address.resource && action.action == address.action
                    })
            })
        });
        (!eager)
            .then(|| self.directory_root_for(caller, peer_id))
            .flatten()
    }

    pub(super) fn invoke_directory(
        self: &Arc<Self>,
        caller: &Caller,
        root: ActionDirectoryRoot,
        origin: Option<&super::super::published_invocation::InvocationCaller>,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let unavailable = || ActionHostError::Unavailable {
            reason: "the invocation ledger is unavailable".into(),
        };
        // A request naming no revision acts on the directory as it is now:
        // its owner resolves it so, in the same turn this broker admits it,
        // instead of the caller first reading the revision across the same
        // two hops.
        let revision = match request.revision.as_deref() {
            None => String::new(),
            Some("") => {
                return Pending::ready(Err(ActionHostError::InvalidRequest {
                    reason:
                        "an empty revision names none; omit it to act on the directory as it is now"
                            .into(),
                }));
            }
            Some(revision) => revision.to_owned(),
        };
        let argument = request.argument.to_slot();
        let address = &request.target.address;
        if let Some(reason) = argument.as_deref().and_then(oversized_argument) {
            return Pending::ready(Err(ActionHostError::InvalidRequest { reason }));
        }
        if validate_directory_target_input(
            &root.cluster_id,
            &revision,
            &address.resource,
            &address.action,
            argument.as_deref(),
            None,
        )
        .is_err()
        {
            return Pending::ready(Err(ActionHostError::InvalidRequest {
                reason: "the directory target or argument is malformed".into(),
            }));
        }
        let reservation = match self.remote_invocations.lock() {
            Ok(mut ledger) => ledger.reserve(caller, request),
            Err(_) => Err(unavailable()),
        };
        match reservation {
            Err(error) => return Pending::ready(Err(error)),
            Ok(Reservation::Admitted) => {
                return self.admitted_receipt(caller, request.invocation);
            }
            Ok(Reservation::Reserved) => {}
        }
        let resolved = self.action_directory_request_queue().try_resolve_pending(
            self.caller_actor(caller, &root.cluster_id),
            &root.cluster_id,
            root.peer_id,
            revision,
            &address.resource,
            &address.action,
        );
        let resolved = match resolved {
            Ok(resolved) => resolved,
            Err(error) => {
                if let Ok(mut ledger) = self.remote_invocations.lock() {
                    ledger.abandon(&request.invocation);
                }
                return Pending::ready(Err(bridge_error(error)));
            }
        };
        let (completer, admitted) = pending();
        let state = Arc::clone(self);
        let caller = caller.clone();
        let request = request.clone();
        let origin = origin.cloned();
        // Runs on the broker's local set in the same turn the owner answered,
        // so nothing can change between resolution and admission.
        resolved.then(move |reply| {
            // A caller that stopped waiting (its admission deadline passed)
            // has already been told the action did not start; keep it so.
            let staged = if completer.is_abandoned() {
                Err(ActionHostError::Unavailable {
                    reason: "the caller stopped waiting for admission".into(),
                })
            } else {
                state.start_resolved(
                    &caller,
                    &root,
                    &request,
                    origin.as_ref(),
                    argument.as_deref(),
                    reply,
                )
            };
            // Answered once admitted durably and delivered.
            state
                .finish_started(request.invocation, staged.map(Started::Staged))
                .then(move |receipt| {
                    completer.complete(receipt.unwrap_or_else(|| {
                        Err(ActionHostError::Unavailable {
                            reason: "the admission was dropped".into(),
                        })
                    }));
                });
        });
        admitted
    }

    fn start_resolved(
        &self,
        caller: &Caller,
        root: &ActionDirectoryRoot,
        request: &InvokeRequest,
        origin: Option<&super::super::published_invocation::InvocationCaller>,
        argument: Option<&str>,
        reply: Option<ActionDirectoryBridgeReply>,
    ) -> Result<super::super::StagedJob, ActionHostError> {
        let stale = || ActionHostError::StaleRevision;
        let (row, resolved) = match reply {
            Some(Ok(ActionDirectoryBridgeResponse::Resolution(
                runtime_collab::ActionDirectoryResolution::Found { row, revision },
            ))) => (row, revision),
            Some(Ok(ActionDirectoryBridgeResponse::Resolution(
                runtime_collab::ActionDirectoryResolution::StaleRevision,
            ))) => return Err(stale()),
            Some(Ok(_)) => return Err(ActionHostError::UnknownAction),
            Some(Err(error)) => return Err(bridge_error(error)),
            None => {
                return Err(ActionHostError::Unavailable {
                    reason: "the action directory went away".into(),
                });
            }
        };
        // The admission is bound to the revision the caller chose the target
        // at or, when it named none, the one its owner resolved it against
        // just now: a host refuses to run it once the directory moved on.
        // An owner that names no revision for an empty one resolved nothing
        // the host could check against later.
        let revision = request.revision.clone().or(resolved).ok_or_else(stale)?;
        let offered =
            published_directory_action(&row).map_err(|_| ActionHostError::UnknownAction)?;
        // A person's confirmation covers the action as they were shown it.
        // Without the revision it was shown at, the broker cannot tell that
        // from the action as it is now, so only reads and plain edits act on
        // the directory as it is now.
        if request.revision.is_none()
            && offered.contract.confirmation_for_argument(argument)
                != tilo_actions::ConfirmationPolicy::None
        {
            return Err(ActionHostError::InvalidRequest {
                reason: CONFIRMED_ACTION_NEEDS_REVISION.into(),
            });
        }
        let actor = self.caller_actor(caller, &root.cluster_id);
        let token = self.confirmation_token(
            root.peer_id,
            &root.app_id,
            &offered,
            argument,
            &actor,
            request.confirmation.as_ref(),
            &self.witness_binding(actor.id(), &root.cluster_id, request),
        )?;
        let surface = PublishedSurface {
            peer_id: root.peer_id,
            app_id: root.app_id.clone(),
            route: CrdtRoute {
                app_id: root.app_id.clone(),
                cluster_id: root.cluster_id.clone(),
                record_id: root.record_id.clone(),
            },
            actions: vec![offered.clone()],
            live: true,
        };
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| ActionHostError::Unavailable {
                reason: "invocation admission unavailable".into(),
            })?;
        if request
            .target
            .provider
            .starts_with(super::super::canonical_route::PROVIDER_PREFIX)
            && self.canonical_token_directory(&request.target.provider)? != *root
        {
            return Err(ActionHostError::UnknownAction);
        }
        self.dispatch_invocation_from_offered_owned(
            root.peer_id,
            (surface, offered),
            InvocationInputs {
                original_request: Some(request),
                caller: Some(caller),
                requested: Some(request.invocation),
                execution: super::super::ExecutionScope::for_call(caller, request.invocation),
                argument,
                actor: &actor,
                confirmation_token: token.as_deref(),
                context: request.context.clone(),
            },
            super::super::AdmissionProvenance {
                durable: None,
                revision: Some(&revision),
                origin: origin.cloned(),
                retained_time: None,
            },
        )
        .map_err(refusal)
    }
}

/// A directory row, already validated and permission-checked on the local
/// set, as a catalogue row.
fn directory_row(
    root: &ActionDirectoryRoot,
    row: &runtime_collab::ActionDirectoryRow,
) -> Option<CatalogueRow> {
    let action = if row.action.is_some() {
        Some(descriptor(&published_directory_action(row).ok()?))
    } else {
        None
    };
    Some(CatalogueRow {
        provider: root.peer_id.to_string(),
        app_id: root.app_id.clone(),
        parent_resource: row.parent_resource.clone(),
        resource: CatalogueResource {
            id: row.resource.clone(),
            kind: row.resource_kind.clone(),
            label: row.resource_label.clone(),
            presentation: row.presentation.clone(),
        },
        action,
        has_children: row.has_children,
        visibility: row.visibility.clone(),
    })
}

fn directory_page(
    root: &ActionDirectoryRoot,
    reply: ActionDirectoryBridgeReply,
) -> Result<CataloguePage, ActionHostError> {
    match reply.map_err(bridge_error)? {
        ActionDirectoryBridgeResponse::Page(page) => Ok(CataloguePage {
            locator: None,
            rows: page
                .rows
                .iter()
                .filter_map(|row| directory_row(root, row))
                .collect(),
            revision: page.revision,
            providers: Vec::new(),
            next_cursor: page.next_cursor,
            exhausted: page.exhausted,
            stale_cursor: page.stale_cursor,
            budget_exhausted: page.budget_exhausted,
        }),
        ActionDirectoryBridgeResponse::Resolution(resolution) => {
            let mut page = CataloguePage {
                exhausted: true,
                ..CataloguePage::default()
            };
            match resolution {
                runtime_collab::ActionDirectoryResolution::Found { row, .. } => {
                    page.rows.extend(directory_row(root, &row));
                }
                runtime_collab::ActionDirectoryResolution::StaleRevision => {
                    page.stale_cursor = true;
                }
                runtime_collab::ActionDirectoryResolution::NotFound
                | runtime_collab::ActionDirectoryResolution::Unavailable => {}
            }
            Ok(page)
        }
        // Only a start request answers with a started action; a query or
        // resolution never does.
        ActionDirectoryBridgeResponse::StartedDirectoryAction(_) => {
            Err(ActionHostError::Unavailable {
                reason: "the action directory answered a different request".into(),
            })
        }
    }
}

pub(super) fn bridge_error(error: ActionDirectoryBridgeError) -> ActionHostError {
    match error {
        ActionDirectoryBridgeError::InvalidRequest => ActionHostError::InvalidRequest {
            reason: "the directory request is malformed".into(),
        },
        ActionDirectoryBridgeError::TimedOut => ActionHostError::Unavailable {
            reason: "the app has not answered its action directory for a long time: it is hung"
                .into(),
        },
        other => ActionHostError::Unavailable {
            reason: format!("the action directory is {other:?}"),
        },
    }
}

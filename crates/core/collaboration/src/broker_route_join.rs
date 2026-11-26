//! A route beside an app's own joins on a task of its own, within a bound:
//! the app's connection goes on while it joins, and a join that fails or
//! passes the bound ends with a reason the app shows.

use super::{
    BridgeState, DocumentRoute, InboundBridge, KeySignal, RouteSpec, SdkCommand, SdkJoinContext,
    SdkRoute, committed_document, follow_channel_keys, record_sync, spawn_content_stream,
    spawn_context_stream, take_invocations_once_current, tokio_mpsc,
};
use crate::time::Duration;
use tilo_lock::MutexRecover as _;

/// The longest a route beside the app's own may take to join, its first
/// committed read included, before it ends unjoined with a reason the app
/// shows. The app's own route reports what holds its join up instead
/// ([`super::report_join_hold`]).
#[cfg(not(test))]
pub(super) const ROUTE_JOIN_WAIT: Duration = Duration::from_secs(60);
#[cfg(test)]
pub(super) const ROUTE_JOIN_WAIT: Duration = Duration::from_secs(2);

/// What a route's join running beside the shovel holds of the shovel's own
/// connection and identity.
pub(super) struct JoinInputs {
    pub(super) asks_channel_keys: bool,
    pub(super) key_signals: tokio_mpsc::UnboundedSender<KeySignal>,
    pub(super) client: runtime_collab::CollabClient,
    pub(super) member_kind: &'static str,
    pub(super) inbound_tx: InboundBridge,
}

/// A route's join that ended: the route, or why it did not join.
pub(super) type JoinedRoute = (RouteSpec, Result<SdkRoute, String>);

/// Join `spec`'s route on its own task, and answer on `joined` with the route
/// or why it did not join, within [`ROUTE_JOIN_WAIT`]: a route whose broker,
/// or whose first read, never answers ends as unjoined rather than holding
/// the app's connection.
pub(super) fn start_route_join(
    inputs: JoinInputs,
    spec: RouteSpec,
    resource_type: String,
    joined: tokio_mpsc::UnboundedSender<JoinedRoute>,
) {
    tokio::task::spawn_local(async move {
        let context = SdkJoinContext {
            asks_channel_keys: inputs.asks_channel_keys,
            key_signals: &inputs.key_signals,
            client: &inputs.client,
            member_kind: inputs.member_kind,
            inbound_tx: &inputs.inbound_tx,
        };
        let result = match tokio::time::timeout(
            ROUTE_JOIN_WAIT,
            join_route(&context, &spec, &resource_type),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(format!(
                "{}: the record did not load within {} s",
                crate::ROUTE_JOIN_FAILED,
                ROUTE_JOIN_WAIT.as_secs()
            )),
        };
        let _ = joined.send((spec, result));
    });
}

/// The route `command` is for, when it waits for that route to join, and
/// whether it starts the join itself (a write to a route not joined yet).
pub(super) fn waits_on_route(command: &SdkCommand) -> Option<(RouteSpec, bool)> {
    match command {
        SdkCommand::ToRoute(spec, _) => Some((spec.clone(), true)),
        SdkCommand::InstallEncryption { spec, .. } => Some((spec.clone(), false)),
        SdkCommand::Resources { route, .. } => Some((route.clone(), true)),
        _ => None,
    }
}

/// Answer `command`, which waited for a route that did not join, with why:
/// a write leaves its record unpublished, which the record's owner sends whole once the route opens.
pub(super) fn refuse_unjoined(command: SdkCommand, reason: &str, state: &BridgeState, key: &str) {
    let document = command.document_len().is_some();
    match command {
        SdkCommand::InstallEncryption { reply, .. } => {
            let _ = reply.send(Err(reason.to_owned()));
        }
        SdkCommand::Resources { reply, .. } => {
            let _ = reply.try_send(Err(reason.to_owned()));
        }
        _ => {}
    }
    // That record alone is not shared, and its owner sends it whole once the
    // route opens; the app and its other records carry on.
    if document {
        state.route_unpublished(key.to_owned());
    }
}

/// Join `spec`'s route: the broker's channel, its committed read, its
/// content's sealing, its record's changes and its streams. The caller keeps the route it answers.
// Not `Send`: the route state it builds stays on the shovel thread's
// `LocalSet` (see `run_shovel`).
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
pub(super) async fn join_route(
    context: &SdkJoinContext<'_>,
    spec: &RouteSpec,
    resource_type: &str,
) -> Result<SdkRoute, String> {
    let SdkJoinContext {
        asks_channel_keys,
        key_signals,
        client,
        member_kind,
        inbound_tx,
    } = *context;
    let channel_id = spec.channel_id();
    inbound_tx
        .state
        .connection_snapshots
        .lock_recover()
        .remove(&channel_id);
    // Each round trip of the join is marked, so a launch's startup line says which one
    // its wait for the route went to (the route's join has been most of a cold start).
    tilo_startup_metrics::startup_mark("collab-join-start");
    let channel = client.record(&channel_id, resource_type);
    // The first call naming the record is where the host admits the connection to it, so a
    // record the app may not open ends its route here, with the host's reason.
    let sealed = match channel.sealed().await {
        Ok(sealed) => sealed,
        Err(error) => {
            tracing::warn!(
                "collab: opening record {channel_id} failed ({error}); route unavailable"
            );
            return Err(format!("{}: {error}", crate::ROUTE_JOIN_FAILED));
        }
    };
    tilo_startup_metrics::startup_mark("collab-join-channel");

    // A chain's head names whichever of the chain's parts is the newest and holds nothing of its
    // own: opening it asks the workspace which part that is, and the part arrives on its own route.
    // So this route keeps no document, no channel keys and no streams of its own, and the chain's
    // later parts arrive on theirs.
    if spec
        .document
        .as_deref()
        .and_then(collab_types::document::DocumentId::parse)
        .is_some_and(|record| record.is_chain_head())
    {
        committed_document::ask_chain_head(&channel, spec, inbound_tx).await?;
        tilo_startup_metrics::startup_mark("collab-join-chain-head");
        return Ok(SdkRoute {
            _moderation: None,
            document: DocumentRoute {
                spec: spec.clone(),
                channel,
                frontier: record_sync::Frontier::default(),
                encryption: std::sync::Arc::new(std::sync::Mutex::new(
                    super::super::route_keys::RouteKeys::plain(),
                )),
            },
            workspace_controls: std::collections::HashMap::new(),
            shown_actions: None,
            _record: None,
        });
    }
    let committed = committed_document::install(
        &channel,
        spec,
        inbound_tx,
        committed_document::NativeReadJoin {
            client: client.clone(),
            member_kind: member_kind.into(),
            resource_type: resource_type.into(),
        },
    )
    .await?;
    tilo_startup_metrics::startup_mark("collab-join-committed-read");
    if let Ok(runtime_collab::committed_document::Reply::ConnectionSnapshot(snapshot)) = channel
        .committed_document(&runtime_collab::committed_document::Request::ConnectionSnapshot)
        .await
    {
        if snapshot.app_id == spec.app_id
            && snapshot.workspace == spec.cluster_id.to_string()
            && snapshot.document == spec.document.as_deref().unwrap_or("")
        {
            inbound_tx
                .state
                .connection_snapshots
                .lock_recover()
                .insert(channel_id.clone(), snapshot);
        }
    }
    tilo_startup_metrics::startup_mark("collab-join-connection-snapshot");
    if committed {
        // This reply contains only the original host-retained launch instance;
        // a record label or app environment never becomes private authority.
        let instance = match channel
            .committed_document(&runtime_collab::committed_document::Request::PrivateWindowBinding)
            .await
        {
            Ok(runtime_collab::committed_document::Reply::PrivateWindowBinding { instance }) => {
                instance
            }
            _ => None,
        };
        inbound_tx
            .state
            .document_reads
            .bind_private_window(&channel_id, instance)?;
    }

    // The record's changes go to the device's store, which seals and signs
    // them; the content the record references is sealed here, under the
    // record channel's keys, so a route of a certified endpoint asks for them.
    //
    // A cluster-scope route (an empty app id: discovery and presence for
    // every app of the workspace, as an agent joins to learn the catalogue)
    // names no app of its own, so the identity service can never grant it
    // keys: it refuses the question, and a route left waiting for an
    // answer it cannot get asks again on the timer for the whole life of the
    // connection. It carries no record and stores no content, so it asks for
    // no keys and its timer is never armed for it. Nor does it follow the
    // workspace's changes: each app's own routes carry them.
    let cluster_route = spec.app_id.is_empty();
    let sealed = !cluster_route && sealed;
    // A committed route's content stays as its committing host keeps it,
    // unless the workspace seals what it keeps.
    let encrypted = asks_channel_keys && !cluster_route && (sealed || !committed);
    let encryption = std::sync::Arc::new(std::sync::Mutex::new(if encrypted {
        super::super::route_keys::RouteKeys::requested()
    } else {
        super::super::route_keys::RouteKeys::plain()
    }));
    if encrypted {
        follow_channel_keys(channel.clone(), channel_id.clone(), key_signals.clone());
    }
    tilo_startup_metrics::startup_mark("collab-join-window-binding");
    // A route whose record arrives as committed reads follows it through them;
    // any other reads what the store holds of its record as it opens, and again
    // each time the store says it changed.
    let frontier = record_sync::Frontier::default();
    let record = (!committed && !cluster_route).then(|| {
        record_sync::follow(
            channel.clone(),
            spec.clone(),
            frontier.clone(),
            inbound_tx.clone(),
        )
    });
    // The streams are independent: subscribing to them together costs the
    // join one broker round trip instead of one per stream.
    let ((), ()) = tokio::join!(
        spawn_context_stream(
            &channel,
            record.as_ref().map(record_sync::Follower::changed),
            inbound_tx
        ),
        spawn_content_stream(&channel, spec.clone(), inbound_tx),
    );
    tilo_startup_metrics::startup_mark("collab-join-streams");
    take_invocations_once_current(channel.clone(), spec.clone(), inbound_tx.clone());

    // Each of these queues is optional: without it the channel runs, and the
    // warn the `map_err` closure writes says which feature it lost and why.
    let moderation =
        super::super::external_moderation::register(channel_id.clone(), channel.clone())
            .map_err(|error| {
                tracing::warn!(%error, "collab: external moderation queue unavailable");
                error
            })
            .ok();
    Ok(SdkRoute {
        _moderation: moderation,
        document: DocumentRoute {
            spec: spec.clone(),
            channel,
            frontier,
            encryption,
        },
        workspace_controls: std::collections::HashMap::new(),
        shown_actions: None,
        _record: record,
    })
}

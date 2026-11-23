//! A route beside an app's own joins on a task of its own, within a bound:
//! the app's connection goes on while it joins, and a join that fails or
//! passes the bound ends with a reason the app shows.

use super::{
    BridgeState, DocumentRoute, InboundBridge, KeySignal, RouteLabel, RouteSpec, SdkCommand,
    SdkJoinContext, SdkRoute, broker_member_kind, change_waits, committed_document,
    follow_channel_keys, native_media, spawn_action_catalogue_stream, spawn_content_stream,
    spawn_context_stream, spawn_op_stream, spawn_presence_stream, spawn_realtime_stream,
    take_invocations_once_current, tokio_mpsc,
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
pub(super) const ROUTE_JOIN_WAIT: Duration = Duration::from_secs(5);

/// What a route's join running beside the shovel holds of the shovel's own
/// connection and identity.
pub(super) struct JoinInputs {
    pub(super) asks_channel_keys: bool,
    pub(super) key_signals: tokio_mpsc::UnboundedSender<KeySignal>,
    pub(super) client: runtime_collab::CollabClient,
    pub(super) presentation: Vec<u8>,
    pub(super) person: Option<std::rc::Rc<super::PersonWorkspace>>,
    pub(super) member_handle: Option<String>,
    pub(super) member_kind: &'static str,
    pub(super) inbound_tx: InboundBridge,
    pub(super) media_routes: native_media::Routes,
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
            presentation: &inputs.presentation,
            person: inputs.person.as_deref(),
            member_handle: inputs.member_handle.as_deref(),
            member_kind: inputs.member_kind,
            inbound_tx: &inputs.inbound_tx,
            media_routes: &inputs.media_routes,
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
        _ => None,
    }
}

/// Answer `command`, which waited for a route that did not join, with why:
/// a write leaves its record unpublished, which the record's owner sends whole once the route opens.
pub(super) fn refuse_unjoined(command: SdkCommand, reason: &str, state: &BridgeState, key: &str) {
    let document = command.document_len().is_some();
    if let SdkCommand::InstallEncryption { reply, .. } = command {
        let _ = reply.send(Err(reason.to_owned()));
    }
    // That record alone is not shared, and its owner sends it whole once the
    // route opens; the app and its other records carry on.
    if document {
        state.route_unpublished(key.to_owned());
    }
}

/// Join `spec`'s route: the broker's channel, its committed read, its writes'
/// sealing and its streams. The caller keeps the route it answers.
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
        presentation,
        person,
        member_handle,
        member_kind,
        inbound_tx,
        media_routes,
    } = *context;
    let channel_id = spec.channel_id();
    inbound_tx
        .state
        .connection_snapshots
        .lock_recover()
        .remove(&channel_id);
    let identity_bytes = match person {
        Some(person) => person.presentation(spec).await.unwrap_or(presentation),
        None => presentation,
    };
    // Each round trip of the join is marked, so a launch's startup line says which one
    // its wait for the route went to (the route's join has been most of a cold start).
    tilo_startup_metrics::startup_mark("collab-join-start");
    let channel = match client
        .join_channel_as_kind(
            &channel_id,
            identity_bytes,
            member_handle.unwrap_or(""),
            broker_member_kind(member_kind),
            resource_type,
        )
        .await
    {
        Ok(channel) => channel,
        Err(error) => {
            tracing::warn!(
                "collab: joining channel {channel_id} failed ({error}); route unavailable"
            );
            return Err(format!("{}: {error}", crate::ROUTE_JOIN_FAILED));
        }
    };
    tilo_startup_metrics::startup_mark("collab-join-channel");

    // Only the explicit agent constructors consume the eager workspace
    // decision catalogue. Ordinary App owners discard that event and expose
    // their own actions through publication and the bounded pull directory;
    // acquiring every other app's inventory can prevent an unrelated app from
    // starting. This selects a consumer, not authority: broker admission and
    // per-action permissions are unchanged.
    if member_kind == "agent" {
        // Queue the first decision baseline before document traffic so an
        // agent cannot start deciding against stale local fallback data.
        spawn_action_catalogue_stream(&channel, inbound_tx).await?;
    }
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
            _checks: None,
            _moderation: None,
            document: DocumentRoute {
                spec: spec.clone(),
                channel,
                encryption: std::sync::Arc::new(std::sync::Mutex::new(
                    super::super::route_keys::RouteKeys::plain(),
                )),
            },
            presence: None,
            workspace_controls: std::collections::HashMap::new(),
            resource_queries: std::collections::HashMap::new(),
            shown_actions: None,
            committed: false,
        });
    }
    let committed = committed_document::install(
        &channel,
        spec,
        inbound_tx,
        committed_document::NativeReadJoin {
            client: client.clone(),
            presentation: identity_bytes.to_vec(),
            member_handle: member_handle.unwrap_or("").into(),
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

    // A workspace that seals its records takes every write encrypted under the
    // record channel's keys, whoever reads the record for the app. A route
    // whose endpoint has no keys to ask for refuses each write there.
    //
    // A cluster-scope route (an empty app id: discovery and presence for
    // every app of the workspace, as an agent joins to learn the catalogue)
    // names no app of its own, so the identity service can never grant it
    // keys: it refuses the question, and a route left waiting for an
    // answer it cannot get asks again on the timer for the whole life of the
    // connection. It carries no record and writes nothing, so it asks for no
    // keys and reads nothing about sealing: it stays plaintext, and its
    // timer is never armed for it. Nor does it follow the workspace's
    // changes: the broker hands a route of no app every app's changes, sealed
    // under keys it never holds, so it takes no op stream and asks for no
    // snapshot. Each app's own routes carry them.
    let cluster_route = spec.app_id.is_empty();
    let sealed = if cluster_route {
        false
    } else {
        matches!(
            channel
                .committed_document(&runtime_collab::committed_document::Request::Writes)
                .await,
            Ok(runtime_collab::committed_document::Reply::Writes { sealed: true })
        )
    };
    // A committed route's owner admits and applies each edit itself, and a
    // committing host holds no channel keys: it could neither open a sealed
    // edit nor accept cleartext on the route after one. Committed routes
    // therefore stay cleartext end to end.
    let encrypted = asks_channel_keys && !cluster_route && (sealed || !committed);
    let encryption = std::sync::Arc::new(std::sync::Mutex::new(if encrypted {
        super::super::route_keys::RouteKeys::requested()
    } else if sealed {
        tracing::warn!(
            app = %spec.app_id,
            workspace = %spec.cluster_id,
            "collab: this workspace seals every write and this connection holds no channel keys; its writes are refused"
        );
        super::super::route_keys::RouteKeys::unsealable()
    } else {
        super::super::route_keys::RouteKeys::plain()
    }));
    if encrypted {
        follow_channel_keys(channel.clone(), channel_id.clone(), key_signals.clone());
    }
    tilo_startup_metrics::startup_mark("collab-join-window-binding");
    change_waits::follow(&spec.app_id, &channel, inbound_tx);
    // The streams are independent: subscribing to them together costs the
    // join one broker round trip instead of one per stream.
    let ((), (), (), (), presence) = tokio::join!(
        async {
            if !committed && !cluster_route {
                spawn_op_stream(
                    &channel,
                    RouteLabel::of(spec),
                    inbound_tx,
                    std::sync::Arc::clone(&encryption),
                    encrypted.then(|| (channel_id.clone(), key_signals.clone())),
                )
                .await;
            }
        },
        spawn_context_stream(&channel, spec.clone(), inbound_tx),
        spawn_realtime_stream(&channel, inbound_tx),
        spawn_content_stream(&channel, spec.clone(), inbound_tx),
        spawn_presence_stream(&channel, inbound_tx),
    );
    tilo_startup_metrics::startup_mark("collab-join-streams");
    take_invocations_once_current(channel.clone(), spec.clone(), inbound_tx.clone());
    // Subscriptions are installed first so an existing holder's response
    // cannot race ahead of our document stream.
    if !committed
        && !cluster_route
        && let Err(error) = channel.request_document_snapshot().await
    {
        tracing::warn!(%error, "collab: late-join snapshot request failed");
    }

    // Each of these queues is optional: without it the channel runs, and the
    // warn the `map_err` closure writes says which feature it lost and why.
    let checks = super::super::approved_checks::register(channel_id.clone(), channel.clone())
        .map_err(|error| {
            tracing::warn!(%error, "collab: approved project check queue unavailable");
            error
        })
        .ok();
    let moderation =
        super::super::external_moderation::register(channel_id.clone(), channel.clone())
            .map_err(|error| {
                tracing::warn!(%error, "collab: external moderation queue unavailable");
                error
            })
            .ok();
    media_routes.insert(channel_id.clone(), channel.clone());
    #[cfg(feature = "audio")]
    if spec.document.is_none()
        && member_kind == "person"
        && crate::process::environment()
            .identity
            .collab_service_configured
    {
        super::audio_output::install(channel.clone());
    }
    Ok(SdkRoute {
        _checks: checks,
        _moderation: moderation,
        document: DocumentRoute {
            spec: spec.clone(),
            channel,
            encryption,
        },
        presence,
        workspace_controls: std::collections::HashMap::new(),
        resource_queries: std::collections::HashMap::new(),
        shown_actions: None,
        committed,
    })
}

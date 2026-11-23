//! Asking for routes' channel keys: an identity service, or the keys an
//! endpoint with none was given, answers each route's question off the
//! shovel thread, and the answer updates the route's keys.

use super::{DocumentRoute, EndpointChannelKeys, SdkCertifiedIdentity};

/// The keys the identity service answered, or, when it could not answer,
/// that they are unavailable and the answer was not the channel service's.
pub(super) fn answered_keys(
    reply: Result<tilo_identity_client::wire::ChannelKeysReply, runtime_portal::NotAvailable>,
    generation: u64,
) -> KeysAnswer {
    match reply {
        Ok(reply) => KeysAnswer {
            reply,
            answered: true,
        },
        Err(error) => {
            tracing::warn!(?error, "collab: identity service refused channel keys");
            KeysAnswer {
                reply: tilo_identity_client::wire::ChannelKeysReply {
                    access: tilo_identity_client::wire::KeyAccess::Unavailable,
                    generation,
                    keys: None,
                },
                answered: false,
            }
        }
    }
}

/// One route's answer to a key question.
pub(super) struct KeysAnswer {
    reply: tilo_identity_client::wire::ChannelKeysReply,
    /// The channel service answered (rather than the identity service
    /// refusing or failing): it then tells the route when this answer may
    /// change, and the endpoint need not ask on a timer.
    answered: bool,
}

/// Take the identity service's `reply` for `route`, asked at `generation`.
/// Returns whether the route's state or keys changed.
pub(super) fn update_route_keys(
    route: &DocumentRoute,
    answer: KeysAnswer,
    generation: u64,
) -> bool {
    let Ok(mut keys) = route.encryption.lock() else {
        return false;
    };
    let state = keys.state();
    let KeysAnswer { reply, answered } = answer;
    keys.update(reply.access, reply.generation, reply.keys.as_deref());
    keys.set_answered(answered);
    hold_content_secret(&route.spec, &keys);
    keys.generation() != generation || keys.state() != state
}

/// Let the content `spec`'s record references be sealed under the secret
/// of the members `keys` belong to, while they hold one.
pub(super) fn hold_content_secret(spec: &crate::RouteSpec, keys: &crate::route_keys::RouteKeys) {
    let link = crate::document_content::ContentLink::of(spec);
    link.hold_secret(keys.content_secret());
    let document = spec.document.as_deref().unwrap_or_default();
    if collab_types::validator::requires_independent_document_keys(&spec.app_id, document) {
        link.hold_document_keys(spec, keys.document_keys());
    }
}

/// One route's question to the identity service, taken off the route so it
/// can be asked on another thread.
pub(super) struct KeyQuery {
    route: String,
    workspace_id: String,
    app_id: String,
    document_id: String,
    generation: u64,
}

impl KeyQuery {
    pub(super) fn of(route: &DocumentRoute, generation: u64) -> Self {
        Self {
            route: route.spec.channel_id(),
            workspace_id: route.spec.cluster_id.to_string(),
            app_id: route.spec.app_id.clone(),
            document_id: route.spec.document.clone().unwrap_or_default(),
            generation,
        }
    }
}

/// The identity service's answers to one round of key questions: by route,
/// with the generation each was asked at.
pub(super) type FetchedKeys = Vec<(String, u64, KeysAnswer)>;

/// A round of key questions in flight on a blocking thread.
pub(super) type KeyFetch = std::pin::Pin<Box<dyn std::future::Future<Output = FetchedKeys>>>;

/// Ask a certified identity service `queries` on a blocking thread: its
/// socket answers as slowly as the service does, and the shovel thread
/// carries every route's ops meanwhile.
pub(super) async fn certified_channel_keys(
    identity: std::sync::Arc<dyn SdkCertifiedIdentity>,
    queries: Vec<KeyQuery>,
) -> FetchedKeys {
    endpoint_channel_keys(
        std::sync::Arc::new(
            move |workspace_id: &str, app_id: &str, document_id: &str, generation: u64| {
                identity.group_keys(workspace_id, app_id, document_id, generation)
            },
        ),
        queries,
    )
    .await
}

/// Ask `keys` `queries` on a blocking thread, as
/// [`certified_channel_keys`] asks an identity service.
pub(super) async fn endpoint_channel_keys(
    keys: EndpointChannelKeys,
    queries: Vec<KeyQuery>,
) -> FetchedKeys {
    tokio::task::spawn_blocking(move || {
        queries
            .into_iter()
            .map(|query| {
                let reply = keys(
                    &query.workspace_id,
                    &query.app_id,
                    &query.document_id,
                    query.generation,
                );
                (
                    query.route,
                    query.generation,
                    answered_keys(reply, query.generation),
                )
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

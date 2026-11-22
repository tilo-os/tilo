//! A vault item's text for one of Browser's page actions, asked for on Browser's own
//! connection and waited for off its dispatch.

use std::sync::Arc;

use super::{BrokerError, wire};

/// What an app is told when its connection is not a person's own.
const NOT_A_PERSONS_APP: &str = "Only a person's own app keeps or uses an account's password.";

/// What an app is told when this device keeps no workspace vault to answer from.
const NO_VAULT_HERE: &str = "This device cannot reach the workspace's passwords right now.";

/// What an app that is not Browser is told when it asks for a vault item's text.
const ITEMS_ARE_BROWSERS: &str =
    "Only Browser has a field of a page filled from a vault item, so nothing was released.";

/// Answer an item's use ([`runtime_collab::BrowserServiceSessionCommand::UseItem`]) from an
/// app's own route. `None` for every other command, which is answered at once
/// ([`super::delegate_browser_service_session_command`]).
///
/// Whoever keeps the vault is asked outside this process. So this hands back the answer to
/// wait for and returns: the connection's answer waits for it, and the connection's dispatch
/// never does. Whatever the service decides is an answer of its own, a refusal with its
/// reason among them. A text leaves only where the connection and the service's leave for
/// Browser still stand once it has come, and only as the answer to the page action that
/// asked.
pub(super) fn delegate_item_use(
    service: Option<&Arc<dyn crate::apps::services::BrowserServiceSessionServices>>,
    context: &crate::apps::services::BrowserServiceSessionRequestContext,
    bytes: &[u8],
) -> Option<Result<wire::Answer<zeroize::Zeroizing<Vec<u8>>>, BrokerError>> {
    use runtime_collab::BrowserServiceSessionReply as Reply;
    let refused = || BrokerError::failed("private service-session request refused".into());
    let command = runtime_collab::BrowserServiceSessionCommand::decode(bytes).ok()?;
    if command.account_secret() {
        return Some(delegate_account_secret(service, context, command));
    }
    let invocation = command.item_use()?;
    if context.workspace_id().is_empty()
        || context.person().is_empty()
        || context.revalidate().is_err()
    {
        return Some(Err(refused()));
    }
    let service = service.cloned();
    let answering: crate::apps::services::ItemAnswering =
        if context.app_id() == tilo_app_id::apps::BROWSER {
            match &service {
                Some(service) => service.item_use(context, command.clone()),
                None => return Some(Err(refused())),
            }
        } else {
            // Whoever keeps the vault is never asked for another app.
            Box::pin(std::future::ready(Ok(Reply::item_refused(
                invocation,
                ITEMS_ARE_BROWSERS,
            ))))
        };
    let context = context.clone();
    let answer: wire::Answer<zeroize::Zeroizing<Vec<u8>>> = Box::pin(async move {
        let reply = answering.await.map_err(|_| refused())?;
        context.revalidate().map_err(|_| refused())?;
        if matches!(reply, Reply::ItemText { .. })
            && service
                .as_ref()
                .is_none_or(|service| service.authorize_command(&context, &command).is_err())
        {
            return Err(refused());
        }
        if !reply.matches(&command) {
            return Err(refused());
        }
        reply.encode().map_err(|_| refused())
    });
    Some(Ok(answer))
}

/// Answer an account's password kept or released
/// ([`runtime_collab::BrowserServiceSessionCommand::account_secret`]) from an app's own route,
/// once the service read or wrote the workspace's vault on this device. The app asking is
/// decided before and again after, as every account-secret request is; what the service
/// answers leaves only as the answer to that same request.
fn delegate_account_secret(
    service: Option<&Arc<dyn crate::apps::services::BrowserServiceSessionServices>>,
    context: &crate::apps::services::BrowserServiceSessionRequestContext,
    command: runtime_collab::BrowserServiceSessionCommand,
) -> Result<wire::Answer<zeroize::Zeroizing<Vec<u8>>>, BrokerError> {
    // Each refusal says why, for the person whose app asked: the app shows it as the reason
    // their password was not kept or used.
    let refused = |reason: String| {
        tracing::warn!(app = context.app_id(), %reason, "an account's password was refused to an app");
        BrokerError::failed(reason)
    };
    if context.workspace_id().is_empty() || context.person().is_empty() {
        return Err(refused(NOT_A_PERSONS_APP.to_owned()));
    }
    context.revalidate().map_err(refused)?;
    let service = service
        .cloned()
        .ok_or_else(|| refused(NO_VAULT_HERE.to_owned()))?;
    service
        .authorize_command(context, &command)
        .map_err(refused)?;
    let answering = service.account_secret(context, command.clone());
    let context = context.clone();
    Ok(Box::pin(async move {
        let refused = |reason: String| {
            tracing::warn!(app = context.app_id(), %reason, "an account's password was refused to an app");
            BrokerError::failed(reason)
        };
        // What the service says is said to the app: a refusal designed for its person.
        let reply = answering.await.map_err(BrokerError::failed)?;
        context.revalidate().map_err(refused)?;
        service
            .authorize_command(&context, &command)
            .map_err(refused)?;
        if !reply.matches(&command) {
            return Err(refused(NO_VAULT_HERE.to_owned()));
        }
        reply.encode().map_err(|error| refused(error.to_string()))
    }))
}

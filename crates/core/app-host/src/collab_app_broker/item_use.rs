//! A vault item's text for one of Browser's page actions, asked for on Browser's own
//! connection and waited for off its dispatch.

use std::sync::Arc;

use super::{BrokerError, wire};
use tilo_workspace_service::workspaces::account_logins::VAULT_NOT_HERE;

/// What an app is told when its connection is not a person's own.
const NOT_A_PERSONS_APP: &str = "Only a person's own app keeps or uses an account's password.";

/// What an app is told when its access ended while it asked. Why it ended is the workspace's
/// to know, so it is logged and not passed to the app.
const ACCESS_ENDED: &str = "This app's access ended, so the request was refused.";

/// What an app that is not Browser is told when it asks for one of the workspace's website
/// sessions.
const SESSIONS_ARE_BROWSERS: &str = "Only Browser keeps the workspace's website sessions.";

/// What an app that is not Browser is told when it asks for a vault item's text.
const ITEMS_ARE_BROWSERS: &str =
    "Only Browser has a field of a page filled from a vault item, so nothing was released.";

/// Answer an item's use ([`runtime_collab::BrowserServiceSessionCommand::UseItem`]), an
/// account's password kept or released, or one of the workspace's website sessions, from an
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
    service: Option<&Arc<dyn crate::services::BrowserServiceSessionServices>>,
    context: &crate::services::BrowserServiceSessionRequestContext,
    bytes: &[u8],
) -> Option<Result<wire::Answer<zeroize::Zeroizing<Vec<u8>>>, BrokerError>> {
    use runtime_collab::BrowserServiceSessionReply as Reply;
    let refused = || BrokerError::failed("private service-session request refused".into());
    let command = runtime_collab::BrowserServiceSessionCommand::decode(bytes).ok()?;
    if command.account_secret() || command.website_session() {
        return Some(delegate_answered_later(service, context, command));
    }
    let invocation = command.item_use()?;
    if context.workspace_id().is_empty()
        || context.person().is_empty()
        || context.revalidate().is_err()
    {
        return Some(Err(refused()));
    }
    let service = service.cloned();
    let answering: crate::services::ItemAnswering =
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
/// ([`runtime_collab::BrowserServiceSessionCommand::account_secret`]), or one of the
/// workspace's website sessions listed, resolved or forgotten
/// ([`runtime_collab::BrowserServiceSessionCommand::website_session`]), from an app's own
/// route, once the service read or wrote the workspace's vault and accounts on this device.
/// Website sessions are Browser's alone. The app asking is decided before and again after;
/// what the service answers leaves only as the answer to that same request.
fn delegate_answered_later(
    service: Option<&Arc<dyn crate::services::BrowserServiceSessionServices>>,
    context: &crate::services::BrowserServiceSessionRequestContext,
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
    context.revalidate().map_err(|reason| {
        tracing::warn!(app = context.app_id(), %reason, "an app's access ended before its request");
        refused(ACCESS_ENDED.to_owned())
    })?;
    if command.browser_only() && context.app_id() != tilo_app_id::apps::BROWSER {
        return Err(refused(SESSIONS_ARE_BROWSERS.to_owned()));
    }
    let service = service
        .cloned()
        .ok_or_else(|| refused(VAULT_NOT_HERE.to_owned()))?;
    service
        .authorize_command(context, &command)
        .map_err(refused)?;
    let answering = service.answered_later(context, command.clone());
    let context = context.clone();
    Ok(Box::pin(async move {
        let refused = |reason: String| {
            tracing::warn!(app = context.app_id(), %reason, "an account's password was refused to an app");
            BrokerError::failed(reason)
        };
        // What the service says is said to the app: a refusal designed for its person.
        let reply = answering.await.map_err(BrokerError::failed)?;
        context.revalidate().map_err(|reason| {
            tracing::warn!(app = context.app_id(), %reason, "an app's access ended while it was answered");
            refused(ACCESS_ENDED.to_owned())
        })?;
        service
            .authorize_command(&context, &command)
            .map_err(refused)?;
        if !reply.matches(&command) {
            return Err(refused(VAULT_NOT_HERE.to_owned()));
        }
        reply.encode().map_err(|error| refused(error.to_string()))
    }))
}

//! A vault item's text for one of Browser's page actions, asked for on Browser's own
//! connection and waited for off its dispatch.

use std::sync::Arc;

use super::{BrokerError, wire};

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
    service: Option<&Arc<dyn crate::services::BrowserServiceSessionServices>>,
    context: &crate::services::BrowserServiceSessionRequestContext,
    bytes: &[u8],
) -> Option<Result<wire::Answer<zeroize::Zeroizing<Vec<u8>>>, BrokerError>> {
    use runtime_collab::BrowserServiceSessionReply as Reply;
    let refused = || BrokerError::failed("private service-session request refused".into());
    let command = runtime_collab::BrowserServiceSessionCommand::decode(bytes).ok()?;
    let invocation = command.item_use()?;
    if context.workspace_id().is_empty()
        || context.person().is_empty()
        || context.revalidate().is_err()
    {
        return Some(Err(refused()));
    }
    let service = service.cloned();
    let answering: crate::services::ItemAnswering = if context.app_id() == ui::apps::BROWSER {
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

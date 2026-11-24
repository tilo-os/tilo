//! What carries a page's call: the browser that shows the page, never the device that lays its
//! view out.
//!
//! A call is the browser's — its microphone, camera and screen, and the workspace's live
//! session are there — while the view that shows it is laid out where the page's device hosts
//! it. So a step the view asks for goes to the page, over the page's own protocol, and the
//! browser's answer comes back as the status the view reads. Every other device's call is the
//! seat's, as it always was.

use std::sync::Arc;

use runtime_collab::native_media::{REPLY_TIMEOUT, Request, Snapshot};
use tilo_actions::remote::{Caller, Pending};
use tilo_workspace_service::apps::collab_app_broker::{
    NativeMediaFuture, NativeMediaRequestContext, NativeMediaServices,
};

use crate::key_sessions::Pages;

/// This device's apps' calls: a page's view's to the browser that shows it, the rest to `seat`.
pub(crate) struct PageMedia {
    pub(crate) pages: Pages,
    pub(crate) seat: Arc<dyn NativeMediaServices>,
}

impl PageMedia {
    /// What the browser that shows the view of `context` answers `request` with, when a page
    /// shows that view: the same app, workspace and person the page was opened for.
    fn carried_by_page(
        &self,
        context: &NativeMediaRequestContext,
        request: &Request,
    ) -> Option<Pending<Result<Snapshot, String>>> {
        let person = person_of(context.opened_for()?);
        self.pages.ask_call(
            context.workspace_id(),
            context.app_id(),
            &person,
            request.clone(),
        )
    }
}

impl NativeMediaServices for PageMedia {
    fn command(&self, context: NativeMediaRequestContext, request: Request) -> NativeMediaFuture {
        if let Some(answered) = self.carried_by_page(&context, &request) {
            return Box::pin(async move {
                match tokio::time::timeout(REPLY_TIMEOUT, answered).await {
                    Ok(Some(answer)) => answer,
                    // The page went before it answered.
                    Ok(None) => Err("the browser stopped answering the call".to_owned()),
                    // The browser had the step and did not answer: a page that stood still
                    // carries no call, and the view says so.
                    Err(_) => Err("the browser did not answer the call".to_owned()),
                }
            });
        }
        self.seat.command(context, request)
    }

    fn disconnect(&self, peer_id: u64) {
        self.seat.disconnect(peer_id);
    }
}

/// Who is at the view, as the page and the request both name them: the person acting, else the
/// person it acts for, else the actor itself.
fn person_of(caller: &Caller) -> String {
    let acting = caller.acting();
    acting
        .person_id()
        .or_else(|| acting.acted_for_person())
        .map_or_else(|| acting.by.to_string(), ToOwned::to_owned)
}

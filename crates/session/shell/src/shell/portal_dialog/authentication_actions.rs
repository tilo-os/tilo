//! Semantic access to the exact current compositor-owned authentication sheet.
//! A ticket owns the actual request attempt; a reused Polkit cookie is not it.
use super::{
    AuthenticationDecision, AuthenticationRequest, DialogQueueInner, PendingDialog,
    PortalDialogQueue,
};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct AuthenticationTicket {
    pub(crate) generation: uuid::Uuid,
    request: Arc<AuthenticationRequest>,
    owner_uid: libc::uid_t,
}
impl AuthenticationTicket {
    pub(crate) fn request(&self) -> &AuthenticationRequest {
        &self.request
    }
}
fn head(inner: &DialogQueueInner) -> Option<&str> {
    inner
        .active
        .as_deref()
        .filter(|handle| inner.pending.contains_key(*handle))
        .or_else(|| {
            inner
                .order
                .iter()
                .find(|handle| inner.pending.contains_key(*handle))
                .map(String::as_str)
        })
}
impl PortalDialogQueue {
    pub(super) fn authentication_generation(&self, handle: &str) -> Option<uuid::Uuid> {
        let inner = self.inner.lock().ok()?;
        let PendingDialog::Authentication { generation, .. } = inner.pending.get(handle)? else {
            return None;
        };
        Some(*generation)
    }
    pub(crate) fn current_authentication(&self) -> Option<AuthenticationTicket> {
        let inner = self.inner.lock().ok()?;
        let PendingDialog::Authentication {
            request,
            generation,
            reply,
            owner_uid,
        } = inner.pending.get(head(&inner)?)?
        else {
            return None;
        };
        if reply.is_closed() {
            return None;
        }
        Some(AuthenticationTicket {
            generation: *generation,
            request: request.clone(),
            owner_uid: *owner_uid,
        })
    }
    /// The semantic and visual sheet feed the same waiting Polkit/PAM exchange.
    /// This acknowledges delivery only; PAM owns authentication success.
    pub(crate) fn answer_authentication(
        &self,
        ticket: &AuthenticationTicket,
        identity: &str,
        secret: String,
    ) -> bool {
        self.finish_authentication_ticket(ticket, Some((identity, secret)))
    }
    pub(crate) fn cancel_authentication(&self, ticket: &AuthenticationTicket) -> bool {
        self.finish_authentication_ticket(ticket, None)
    }
    fn finish_authentication_ticket(
        &self,
        ticket: &AuthenticationTicket,
        answer: Option<(&str, String)>,
    ) -> bool {
        let answer = answer.map(|(identity, secret)| (identity, zeroize::Zeroizing::new(secret)));
        if ticket.owner_uid != rustix::process::geteuid().as_raw() {
            return false;
        }
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        if head(&inner) != Some(ticket.request.handle.as_str()) {
            return false;
        }
        let Some(PendingDialog::Authentication {
            request,
            generation,
            reply,
            owner_uid,
        }) = inner.pending.get(&ticket.request.handle)
        else {
            return false;
        };
        if *owner_uid != ticket.owner_uid
            || *generation != ticket.generation
            || !Arc::ptr_eq(request, &ticket.request)
            || reply.is_closed()
        {
            return false;
        }
        let decision = match answer {
            Some((identity, secret)) => {
                let Some(identity) = request
                    .identities
                    .iter()
                    .position(|offered| offered == identity)
                else {
                    return false;
                };
                AuthenticationDecision::Answered {
                    identity,
                    secret: secret.to_string(),
                }
            }
            None => AuthenticationDecision::Cancelled,
        };
        let Some(PendingDialog::Authentication { reply, .. }) =
            inner.pending.remove(&ticket.request.handle)
        else {
            return false;
        };
        drop(inner);
        reply.send(decision).is_ok()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn request(handle: &str) -> AuthenticationRequest {
        AuthenticationRequest {
            handle: handle.into(),
            message: "Install software".into(),
            action_id: "org.example.install".into(),
            identities: vec!["admin".into(), "second-admin".into()],
            identity: 0,
            prompt: "Password:".into(),
            echo: false,
            notice: None,
        }
    }
    #[test]
    fn actual_waiter_receives_exact_offered_identity_and_secret() {
        let queue = PortalDialogQueue::default();
        let mut reply = queue
            .enqueue_authentication(request("polkit:attempt"))
            .unwrap();
        let ticket = queue.current_authentication().unwrap();
        assert!(!queue.answer_authentication(&ticket, "unoffered-admin", "private".into()));
        assert!(reply.try_recv().is_err());
        assert!(queue.answer_authentication(&ticket, "second-admin", "private".into()));
        assert_eq!(
            reply.try_recv().unwrap(),
            AuthenticationDecision::Answered {
                identity: 1,
                secret: "private".into()
            }
        );
        assert!(!queue.answer_authentication(&ticket, "admin", "again".into()));
    }
    #[test]
    fn retry_with_same_polkit_cookie_refuses_old_ticket_and_old_cancel() {
        let queue = PortalDialogQueue::default();
        let mut first = queue
            .enqueue_authentication(request("polkit:reused-cookie"))
            .unwrap();
        let old = queue.current_authentication().unwrap();
        assert!(queue.cancel_authentication(&old));
        assert_eq!(first.try_recv().unwrap(), AuthenticationDecision::Cancelled);
        let mut retry = queue
            .enqueue_authentication(request("polkit:reused-cookie"))
            .unwrap();
        let current = queue.current_authentication().unwrap();
        assert_ne!(old.generation, current.generation);
        assert!(!queue.cancel_authentication(&old));
        assert!(!queue.answer_authentication(&old, "admin", "old".into()));
        assert!(retry.try_recv().is_err());
        assert!(queue.answer_authentication(&current, "admin", "current".into()));
        assert_eq!(
            retry.try_recv().unwrap(),
            AuthenticationDecision::Answered {
                identity: 0,
                secret: "current".into()
            }
        );
    }
    #[test]
    fn visual_sheet_cannot_answer_a_semantically_completed_retry_without_pumping() {
        let queue = PortalDialogQueue::default();
        let _first = queue
            .enqueue_authentication(request("polkit:retry"))
            .unwrap();
        let mut controller = super::super::PortalDialogController::new(queue.clone());
        assert!(controller.pump());
        let original = queue.current_authentication().unwrap();
        assert!(queue.answer_authentication(&original, "admin", "first".into()));
        let mut retry = queue
            .enqueue_authentication(request("polkit:retry"))
            .unwrap();
        assert!(controller.view().is_none());
        assert!(!controller.confirm());
        assert!(retry.try_recv().is_err());
        assert!(controller.pump());
        assert!(controller.view().is_some());
    }
    #[test]
    fn active_sheet_head_and_withdrawn_waiter_are_not_skipped() {
        let queue = PortalDialogQueue::default();
        let first = queue
            .enqueue_authentication(request("polkit:first"))
            .unwrap();
        let mut second = queue
            .enqueue_authentication(request("polkit:second"))
            .unwrap();
        let mut controller = super::super::PortalDialogController::new(queue.clone());
        assert!(controller.pump());
        let ticket = queue.current_authentication().unwrap();
        assert_eq!(ticket.request().handle, "polkit:first");
        drop(first);
        assert!(queue.current_authentication().is_none());
        assert!(!queue.answer_authentication(&ticket, "admin", "private".into()));
        assert!(second.try_recv().is_err());
        assert!(queue.cancel("polkit:first"));
        controller.pump();
        assert_eq!(
            queue.current_authentication().unwrap().request().handle,
            "polkit:second"
        );
    }
    #[test]
    fn ticket_owner_uid_must_equal_the_actual_kernel_session_uid() {
        let queue = PortalDialogQueue::default();
        let mut reply = queue
            .enqueue_authentication(request("polkit:uid-bound"))
            .unwrap();
        let actual = queue.current_authentication().unwrap();
        let mut foreign = actual.clone();
        foreign.owner_uid ^= 1;
        assert!(!queue.answer_authentication(&foreign, "admin", "private".into()));
        assert!(!queue.cancel_authentication(&foreign));
        assert!(reply.try_recv().is_err());
        assert!(queue.cancel_authentication(&actual));
        assert_eq!(reply.try_recv().unwrap(), AuthenticationDecision::Cancelled);
    }
}

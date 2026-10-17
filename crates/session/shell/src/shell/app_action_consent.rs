//! Asking the person at this machine before an application uses another
//! application's action.
//!
//! An application that declared `[actions] invoke` still holds nothing in a
//! workspace until someone grants it. When it reaches for an action it has
//! no grant for, the broker asks the person who admitted its connection —
//! on a sheet only the compositor draws — instead of refusing silently. The
//! answer becomes an ordinary capability (`Principal::App` subject,
//! `Scope::Action`, the chosen `Expiry`) in the workspace's records,
//! granted by that person. Nobody present means nobody to ask: the request
//! is refused without a sheet.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::oneshot;

use crate::shell::portal_dialog::PortalDialogQueue;
pub use tilo_action_broker::app_actions::{
    AppActionConsent, AppActionConsentPrompter, AppActionConsentRequest,
};

/// How an answer reads on the sheet, in the session's language.
#[must_use]
pub fn consent_label(answer: AppActionConsent) -> String {
    crate::shell::i18n::t(match answer {
        AppActionConsent::Once => "portal-grant-once",
        AppActionConsent::Session => "portal-grant-session",
        AppActionConsent::Always => "portal-grant-always",
        AppActionConsent::Deny => "portal-grant-deny",
    })
}

/// How a risk level reads on the sheet.
#[must_use]
pub fn risk_label(risk: tilo_actions::RiskLevel) -> String {
    crate::shell::i18n::t(match risk {
        tilo_actions::RiskLevel::None => "portal-risk-none",
        tilo_actions::RiskLevel::Low => "portal-risk-low",
        tilo_actions::RiskLevel::Destructive => "portal-risk-destructive",
        tilo_actions::RiskLevel::Privileged => "portal-risk-privileged",
        tilo_actions::RiskLevel::Sensitive => "portal-risk-sensitive",
    })
}

/// Whether the person a request names is there to answer it.
pub type Presence = Arc<dyn Fn(&AppActionConsentRequest) -> bool + Send + Sync>;

/// Asks on the compositor's own sheet.
#[derive(Clone)]
pub struct SheetPrompter {
    queue: PortalDialogQueue,
    present: Presence,
}

impl SheetPrompter {
    #[must_use]
    pub fn new(queue: PortalDialogQueue, present: Presence) -> Self {
        Self { queue, present }
    }

    /// This session's prompter: the sheet the session draws, answered
    /// only by the person sitting at it and only while it is unlocked.
    #[must_use]
    pub fn session() -> Self {
        Self::new(
            PortalDialogQueue::global(),
            Arc::new(|request| session_attended() && is_session_person(request)),
        )
    }
}

impl AppActionConsentPrompter for SheetPrompter {
    fn ask(&self, request: AppActionConsentRequest) -> Option<oneshot::Receiver<AppActionConsent>> {
        if !(self.present)(&request) {
            return None;
        }
        self.queue.enqueue_action_grant(request).ok()
    }
}

/// Whether someone is at this session's unlocked screen. Starts false, so
/// nothing is asked before the compositor says so.
static SESSION_ATTENDED: AtomicBool = AtomicBool::new(false);

/// Record whether someone is at this session's unlocked screen.
pub fn set_session_attended(attended: bool) {
    SESSION_ATTENDED.store(attended, Ordering::Relaxed);
}

pub(crate) fn session_attended() -> bool {
    SESSION_ATTENDED.load(Ordering::Relaxed)
}

/// Whether `request` names the person this session acts for in its room:
/// a collaborator's application reaching this broker is not answered on
/// this screen.
fn is_session_person(request: &AppActionConsentRequest) -> bool {
    crate::shell::state::RoomId::parse(&request.room)
        .is_some_and(|room| crate::shell::state::local_user_id_for_room(&room) == request.person)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(person: &str) -> AppActionConsentRequest {
        AppActionConsentRequest {
            handle: "app-action:1".into(),
            room: uuid::Uuid::nil().to_string(),
            person: person.into(),
            app_id: "so.tilo.shortcuts".into(),
            target_app: "so.tilo.agent".into(),
            resource: "agent.transform:freeform".into(),
            action: "freeform".into(),
            resource_label: "Agent".into(),
            action_label: "Draft with the agent".into(),
            risk: tilo_actions::RiskLevel::Low,
            needs_confirmation: false,
            confirming: false,
            moderation: None,
        }
    }

    #[test]
    fn moderation_sheet_discloses_content_destination_and_identity_for_one_call() {
        use crate::shell::portal_dialog::PortalDialogController;
        use tilo_action_broker::app_actions::{ModerationConsentPreview, ModerationConsentPurpose};
        for purpose in [
            ModerationConsentPurpose::Report,
            ModerationConsentPurpose::Appeal,
        ] {
            let queue = PortalDialogQueue::default();
            let prompter = SheetPrompter::new(queue.clone(), Arc::new(|_| true));
            let mut consent = request("person:ada");
            consent.confirming = true;
            consent.target_app = "so.tilo.store".into();
            consent.moderation = Some(ModerationConsentPreview {
                purpose,
                destination: "https://registry.example".into(),
                owner_root: "ab".repeat(32),
                signing_root: "12".repeat(32),
                target: "case-or-package".into(),
                category: Some("policy".into()),
                content: "The package description is misleading".into(),
                evidence_digest: Some("cd".repeat(32)),
            });
            let mut answer = prompter.ask(consent.clone()).unwrap();
            let mut sheet = PortalDialogController::new(queue);
            assert!(sheet.pump());
            let view = sheet.view().unwrap();
            let detail = view.detail();
            for text in [
                "https://registry.example",
                "case-or-package",
                "policy",
                "The package description is misleading",
            ] {
                assert!(detail.contains(text), "missing {text}: {detail}");
            }
            let unwrapped = detail.replace('\n', "");
            for key in ["ab", "12", "cd"] {
                assert!(unwrapped.contains(&key.repeat(32)));
            }
            assert!(!detail.contains("{\""), "no opaque operation JSON");
            assert_eq!(view.choices().len(), 1, "no lasting grant for disclosure");
            assert!(
                sheet
                    .layout(vello::kurbo::Rect::new(0.0, 0.0, 1280.0, 1080.0))
                    .is_some()
            );
            assert!(
                sheet
                    .layout(vello::kurbo::Rect::new(0.0, 0.0, 480.0, 320.0))
                    .is_none(),
                "never approve a clipped review"
            );
            let laptop = vello::kurbo::Rect::new(0.0, 0.0, 800.0, 600.0);
            let narrow = vello::kurbo::Rect::new(0.0, 0.0, 400.0, 600.0);
            let pages = sheet.review_pages().unwrap();
            assert!(pages.len() > 1);
            let mut disclosed = String::new();
            while sheet.reviewing_more() {
                let layout = sheet.layout(laptop).expect("fits a laptop");
                assert!(laptop.contains_rect(layout.sheet));
                assert!(narrow.contains_rect(sheet.layout(narrow).unwrap().sheet));
                disclosed.push_str(&sheet.displayed_detail());
                assert!(sheet.click(layout.confirm.center(), &layout));
                assert!(matches!(
                    answer.try_recv(),
                    Err(oneshot::error::TryRecvError::Empty)
                ));
            }
            disclosed.push_str(&sheet.displayed_detail());
            assert_eq!(disclosed.replace('\n', ""), detail.replace('\n', ""));
            assert!(sheet.review_back());
            assert!(sheet.reviewing_more());
            assert!(sheet.confirm(), "return to the final page, not approval");
            assert!(matches!(
                answer.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
            assert!(sheet.confirm(), "separate explicit final approval");
            assert_eq!(answer.try_recv().unwrap(), AppActionConsent::Once);
        }
    }

    #[test]
    fn nobody_present_is_never_asked() {
        let queue = PortalDialogQueue::default();
        let absent = SheetPrompter::new(queue.clone(), Arc::new(|_| false));
        assert!(absent.ask(request("person:ada")).is_none());
        let mut sheet = crate::shell::portal_dialog::PortalDialogController::new(queue);
        assert!(!sheet.pump(), "no sheet was queued");
    }

    #[test]
    fn a_present_person_is_asked_on_the_sheet() {
        let queue = PortalDialogQueue::default();
        let prompter = SheetPrompter::new(queue.clone(), Arc::new(|_| true));
        let mut answer = prompter
            .ask(request("person:ada"))
            .expect("the person is asked");
        let mut sheet = crate::shell::portal_dialog::PortalDialogController::new(queue);
        assert!(sheet.pump());
        assert!(sheet.cancel(), "dismissing the sheet answers it");
        assert_eq!(answer.try_recv(), Ok(AppActionConsent::Deny));
    }
}

//! What the programs' questions tell the person without asking: a code on show that they stop the
//! program from, and an allowance they gave beyond this session, which its notice takes back
//! through the portal's own action. Shown on the session's loop, among the notifications.

use std::cell::{OnceCell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_actions::ResourceSpec;
use tilo_service::Services;
use tilo_sheets::word;

use super::ProgramAsks;
use super::kinds::{Grant, Shown};
use crate::platform::notice::{Notice, Notices};

/// What every tag of these notices starts with.
pub(crate) const TAG: &str = "program-ask:";
/// A code on show, by its question's handle.
const SHOWN: &str = "program-ask:shown:";
/// An allowance given, by its app and the device it is kept under.
const GRANTED: &str = "program-ask:granted:";
/// The action that stops the program showing a code.
const STOP: &str = "stop";
/// The action that takes an allowance back.
const TAKE_BACK: &str = "take-back";

/// The notices the programs' questions leave, on the session's loop.
pub(crate) struct ProgramNotices {
    asks: ProgramAsks,
    /// What the session publishes: an allowance is taken back through the portal's own action.
    services: Rc<RefCell<Services>>,
    notices: OnceCell<Rc<dyn Notices>>,
    /// The codes shown now, by handle, as they were told.
    shown: RefCell<BTreeMap<String, Shown>>,
    /// The allowances told, by their tag.
    granted: RefCell<BTreeMap<String, Grant>>,
}

impl ProgramNotices {
    /// The person has `workspace` in front now ([`ProgramAsks::in_front`]).
    pub(crate) fn in_front(&self, workspace: Option<String>) {
        self.asks.in_front(workspace);
    }

    pub(crate) fn new(asks: ProgramAsks, services: Rc<RefCell<Services>>) -> Self {
        Self {
            asks,
            services,
            notices: OnceCell::new(),
            shown: RefCell::default(),
            granted: RefCell::default(),
        }
    }

    /// Tell what is due, take back what went, and carry out what the person chose.
    pub(crate) fn follow(
        &self,
        notifications: &Rc<RefCell<crate::notifications::SessionNotifications>>,
    ) {
        let notices = self
            .notices
            .get_or_init(|| crate::notifications::RoleNotices::tagged(notifications, TAG));
        for (tag, key) in notices.take_chosen_actions() {
            if let Some(handle) = tag.strip_prefix(SHOWN)
                && key == STOP
            {
                self.asks.stop(handle);
            } else if key == TAKE_BACK
                && let Some(grant) = self.granted.borrow_mut().remove(&tag)
            {
                self.take_back(&grant);
                notices.withdraw(&tag);
            }
        }
        let now = self.asks.shown();
        let gone: Vec<String> = self
            .shown
            .borrow()
            .keys()
            .filter(|handle| !now.contains_key(*handle))
            .cloned()
            .collect();
        for handle in gone {
            self.shown.borrow_mut().remove(&handle);
            notices.withdraw(&format!("{SHOWN}{handle}"));
        }
        for (handle, shown) in now {
            if self.shown.borrow().get(&handle) == Some(&shown) {
                continue;
            }
            // Telling it again under the same tag replaces what it said: the digits typed so far.
            let mut notice = Notice::new(
                shown.from.clone(),
                1,
                shown.title.clone(),
                shown.body.clone(),
            )
            .choosing(vec![(STOP.to_owned(), shown.stop.clone())])
            .staying(Some(std::time::Duration::ZERO));
            notice.tag = format!("{SHOWN}{handle}");
            notices.show(&notice);
            self.shown.borrow_mut().insert(handle, shown);
        }
        for grant in self.asks.take_granted() {
            let (title, body) = grant.words();
            let mut notice = Notice::new(grant.app_label.clone(), 1, title, body)
                .choosing(vec![(TAKE_BACK.to_owned(), word("portal-revoke"))]);
            notice.tag = format!("{GRANTED}{}:{}", grant.app_id, grant.device);
            notices.show(&notice);
            self.granted.borrow_mut().insert(notice.tag, grant);
        }
    }

    /// Take the allowance `grant` back, through the action the portal publishes for it.
    fn take_back(&self, grant: &Grant) {
        fn consent<'a>(resources: &'a [ResourceSpec], grant: &Grant) -> Option<&'a ResourceSpec> {
            resources.iter().find_map(|resource| {
                // The allowance taken back is this device's: one kept for another device names
                // that device.
                let presentation = resource.presentation.as_ref();
                let allowed = resource.kind == "portal.consent"
                    && (resource.label == grant.app_id || resource.label == grant.app_label)
                    && presentation.and_then(|presentation| presentation.detail.as_deref())
                        == Some(grant.device.as_str())
                    && presentation.is_none_or(|presentation| presentation.badge.is_none());
                allowed
                    .then_some(resource)
                    .or_else(|| consent(&resource.children, grant))
            })
        }
        let published = self.services.borrow().resources();
        let Some((resource, action)) = consent(&published, grant).and_then(|resource| {
            resource
                .actions
                .first()
                .map(|action| (resource.id.clone(), action.id.clone()))
        }) else {
            tracing::warn!(app = grant.app_id, "no allowance to take back");
            return;
        };
        match self
            .services
            .borrow_mut()
            .invoke(resource.as_str(), &action, &serde_json::Map::new())
        {
            Some(Ok(_)) => {}
            Some(Err(error)) => tracing::warn!(%error, "the allowance was not taken back"),
            None => tracing::warn!(app = grant.app_id, "the portal did not answer"),
        }
    }
}

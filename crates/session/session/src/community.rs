//! The community site this computer is configured with, joined as the session starts.
//! A computer set up for a community is in that community already: nobody wants to be told to join
//! the lounge they were given a computer for. So the session asks the site's door once — the same
//! door, and the same verified invitation, a listing's Join asks — and the workspace comes onto the
//! desk like any other joined one.
//! There is no loop and no timer: one ask per session start, and one more when the machine's
//! network comes back while the join is still waiting, which is the one thing that can make a join
//! that never reached its site land.
//! What the person decided is theirs and not this computer's: it is kept with their workspaces
//! ([`crate::workspaces::Personal`]), which their other devices hold too, so a person who left is
//! not put back by their next computer, nor by their next session here.

use std::cell::RefCell;

use tilo_user_identity::CommunityState;

use crate::directory;
use crate::workspaces::Personal;

/// The community this session joins on its own, and how far it got.
#[derive(Default)]
pub(crate) struct Community {
    /// The site whose home workspace is joined, when one is configured.
    site: Option<url::Url>,
    /// Whether this session asked its site's door already.
    asked: bool,
    /// Whether this machine had a network at the last pass: a network coming back is when a join
    /// still waiting is asked again.
    online: bool,
}

impl Community {
    /// The community `site` names, for a session starting on a computer configured with it.
    pub(crate) const fn for_site(site: Option<url::Url>) -> Self {
        Self {
            site,
            asked: false,
            online: false,
        }
    }
}

impl crate::Shared {
    /// Join the community this computer is configured for, once, as the session starts — or again
    /// when a network comes back and the join has not landed yet.
    pub(crate) fn follow_community(&self) {
        let online = self.readings.borrow().online();
        self.community
            .borrow_mut()
            .follow(&self.directory_store, self.personal.as_ref(), online);
    }
}

impl Community {
    fn follow(
        &mut self,
        store: &directory::Store,
        personal: Option<&RefCell<Personal>>,
        online: bool,
    ) {
        let came_online = online && !self.online;
        self.online = online;
        let Some(site) = self.site.as_ref() else {
            return;
        };
        // A computer with no identity of its own joins nowhere as anybody,
        // and keeps no decision to make.
        let Some(personal) = personal else {
            return;
        };
        match personal.borrow().community(site.as_str()) {
            // The person is in their community, or was and left it: they decided, and their
            // decision stands on every device of theirs.
            Some(CommunityState::Decided) => return,
            // The join is still waiting: it is asked again when a network comes back, and never in
            // between.
            Some(CommunityState::Pending) if self.asked && !came_online => return,
            Some(CommunityState::Pending) | None => {}
        }
        // Written down before the door is asked, so a session that ends mid-join leaves the
        // decision behind it.
        personal
            .borrow_mut()
            .decide_community(site.as_str(), CommunityState::Pending);
        self.asked = true;
        if let Err(error) = crate::platform::directory::join_community(store, site) {
            tracing::warn!(%error, site = %site, "the community was not asked for");
        }
    }
}

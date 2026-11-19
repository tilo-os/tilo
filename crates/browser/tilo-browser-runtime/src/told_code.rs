//! The code that follows what the person's workspace server tells them on a page
//! ([`crate::browser::told_notices`]), reached only through [`TOLD_NOTICES`], declared with
//! `ui::lazy_code!`, so the browser desktop fetches it as its own module once its first frame is
//! up rather than linking it into the page's start.

use tilo_desktop_surface::DesktopHandle;
use tilo_service::notice::Place;

/// Following the person's log of what they were told, and settling what they opened.
#[derive(Clone, Copy)]
pub struct ToldNotices {
    follow: fn(Option<u32>, &str, &str, &DesktopHandle),
    opened: fn(&Place),
}

impl ToldNotices {
    /// Follow what the server the page's peer `peer` holds `workspace` with tells `person`.
    pub(crate) fn follow(
        self,
        peer: Option<u32>,
        workspace: &str,
        person: &str,
        shell: &DesktopHandle,
    ) {
        (self.follow)(peer, workspace, person, shell);
    }

    /// The person chose `place` from a notice.
    pub(crate) fn opened(self, place: &Place) {
        (self.opened)(place);
    }
}

/// The following this build links.
#[must_use]
pub fn builtin_told_notices() -> ToldNotices {
    ToldNotices {
        follow: crate::browser::told_notices::follow,
        opened: crate::browser::told_notices::opened,
    }
}

ui::lazy_code! {
    /// The code that follows what the person is told: linked, or with `deferred-told-notices`
    /// fetched once the page's first frame is up.
    pub static TOLD_NOTICES: ToldNotices = builtin_told_notices, feature = "deferred-told-notices";
}

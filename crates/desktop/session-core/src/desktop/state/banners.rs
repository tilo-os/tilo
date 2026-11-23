//! The banner stack's data: the live banners a host shows, what the person did with one, and the
//! trait a surface of the host's own reads and answers them through. A surface paints a copy; the
//! host owns the log it comes from.

use ui::time::Instant;
use ui::widget::toast::Dwell;

use crate::desktop::notifications::{Notification, NotificationHost, present_banners};
use crate::desktop::state::notices::DesktopNotifications;

/// The margin a panel pinned to a display's top-right corner keeps from its edge: one number for
/// the banner stack, the notification centre and quick settings, so the corner reads as one place
/// rather than three panels that nearly line up. The overlay stack that hangs the centre and
/// quick settings under the bar reads this constant too.
pub const CORNER_INSET: f64 = 8.0;

/// What a surface shows: a copy of the host's live banners and whether the session is locked. A
/// surface ages its copy as it reports to the host ([`BannerAsk::Present`]), so the two agree until
/// the host hands it the next one.
#[derive(Clone, Debug, Default)]
pub struct BannerFeed {
    /// Live banners, oldest first.
    pub banners: Vec<Banner>,
    /// While locked, banners collapse to a redacted count.
    pub locked: bool,
    /// Whether the host's banners were held at its last present.
    pub held: bool,
}

/// One live banner and the host's choices for it.
#[derive(Clone, Debug)]
pub struct Banner {
    pub notification: Notification,
    /// Host-owned choices, as `(key, label)` : the card draws at most two.
    pub actions: Vec<(String, String)>,
}

impl BannerFeed {
    /// The banners `host` shows now.
    pub fn of(host: &dyn NotificationHost) -> Self {
        let log = host.log();
        Self {
            banners: log
                .active()
                .iter()
                .map(|notification| Banner {
                    actions: host.actions_for(notification),
                    notification: notification.clone(),
                })
                .collect(),
            locked: host.session_locked(),
            held: log.held(),
        }
    }

    /// Live banner `id` .
    pub fn banner(&self, id: u64) -> Option<&Banner> {
        self.banners
            .iter()
            .find(|banner| banner.notification.id == id)
    }

    /// A call rings now: an incoming-call banner is up and the session is unlocked. Its card is on
    /// the display in use; the other displays show a quiet edge.
    pub fn call_ringing(&self) -> bool {
        !self.locked && self.banners.iter().any(|banner| banner.notification.rings)
    }

    /// Age the banners as the host does for the same report ([`NotificationLog::present`]).
    /// Whether that changed them.
    ///
    /// [`NotificationLog::present`]: crate::desktop::notifications::NotificationLog::present
    pub fn present(&mut self, now: Instant, on_screen: &[u64], held: bool) -> bool {
        let before: Vec<Dwell> = self.dwells().collect();
        let was_held = self.held;
        present_banners(
            self.banners
                .iter_mut()
                .map(|banner| &mut banner.notification),
            &mut self.held,
            (now, on_screen, held),
        );
        was_held != self.held || !self.dwells().eq(before)
    }

    fn dwells(&self) -> impl Iterator<Item = Dwell> + '_ {
        self.banners.iter().map(|banner| banner.notification.dwell)
    }
}

/// What the person did with the banners, for the host to carry out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BannerAsk {
    /// The surface laid out `on_screen` (newest first) at `now` , and the person `held` it (the
    /// pointer on the stack, keyboard focus in a banner): forward to
    /// [`NotificationLog::present`](crate::desktop::notifications::NotificationLog::present).
    Present {
        now: Instant,
        on_screen: Vec<u64>,
        held: bool,
    },
    /// The card shown in for the banners that did not fit was chosen.
    OpenCenter,
    /// The person selected banner `id` (not its close button).
    Activate(u64),
    /// The person closed banner `id` .
    Dismiss(u64),
    /// The person chose one of banner `id` 's host-owned choices.
    Invoke(u64, String),
    /// The person answered banner `id` 's inline reply with `text` .
    Reply { id: u64, text: String },
}

impl BannerAsk {
    /// Carry the ask out on `host` .
    pub fn answer(self, host: &dyn NotificationHost) {
        match self {
            Self::Present {
                now,
                on_screen,
                held,
            } => host.present(now, &on_screen, held),
            Self::OpenCenter => host.open_center(),
            Self::Activate(id) => host.activate(id),
            Self::Dismiss(id) => host.dismiss(id),
            Self::Invoke(id, key) => host.invoke_action(id, &key),
            Self::Reply { id, text } => host.reply(id, &text),
        }
    }
}

/// The banners of a [`DesktopNotifications`] log, for a banner surface of the host's own.
pub trait Banners {
    /// The banners shown now. Nothing while the centre is open: a banner there would stand over
    /// the rows the person is reading.
    fn feed(&self) -> BannerFeed;
    /// Carry out what the person did on a banner surface of the host's own.
    fn answer(&self, ask: BannerAsk);
}

impl Banners for DesktopNotifications {
    fn feed(&self) -> BannerFeed {
        if self.log.centre_open.get() {
            BannerFeed::default()
        } else {
            BannerFeed::of(self.host())
        }
    }

    fn answer(&self, ask: BannerAsk) {
        ask.answer(self.host());
    }
}

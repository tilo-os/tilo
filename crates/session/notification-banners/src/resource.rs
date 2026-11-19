//! The banners as a resource, where their effects are published, so
//! the person, the palette, an agent and a script close a banner, choose on
//! it, reply to it or answer the call it rings for through the same actions.
//!
//! The banners keep no document. What they show is the desktop's live
//! notifications, lent to them ([`crate::link`]), and published as their
//! live rows; nothing of it is recorded, synced or kept in History.

use ui::framework::{ResourceApp, ViewCx};
use ui::tilo_resource::{self, Effect, Model, resource};
use ui::widget::Element;

use crate::link::BannerService;
use crate::surface::BannerSurface;

/// The banners resource, where their effects and rows are published.
/// It holds nothing.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Banners {}

#[resource(kind = "notifications.banners")]
impl Banners {}

/// What anyone can do with the banners on screen. A banner is named by its
/// id, as its live row gives it; left out, the newest banner (for a call,
/// the ringing call). Opening, choosing, replying and answering run what the
/// sender offered as the person, so whoever acts for them is asked first.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.notification-banners")]
pub enum BannerEffect {
    /// Open notification
    #[effect(kind = "outside")]
    #[effect(own_device, attended, icon = "bell", on = BannerRow)]
    Open {
        #[param("The banner to open; the newest when left out")]
        banner: Option<String>,
    },
    /// Close notification
    #[effect(kind = "outside")]
    #[effect(own_device, icon = "x")]
    Dismiss {
        #[param("The banner to close; the newest when left out")]
        banner: Option<String>,
    },
    /// Choose on notification
    #[effect(kind = "outside")]
    #[effect(own_device, attended)]
    Act {
        #[param("The banner; the newest when left out")]
        banner: Option<String>,
        #[param("The choice, by its key or its label (Snooze, Approve…)")]
        choice: String,
    },
    /// Reply to notification
    #[effect(kind = "outside")]
    #[effect(own_device, attended)]
    Reply {
        #[param("The banner; the newest when left out")]
        banner: Option<String>,
        #[param("What to reply")]
        text: String,
    },
    /// Snooze notification
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Snooze {
        #[param("The banner; the newest when left out")]
        banner: Option<String>,
    },
    /// Focus the newest notification
    #[effect(view, own_device)]
    FocusNewest,
    /// Read notification in full
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Read {
        #[param("The banner; the newest when left out")]
        banner: Option<String>,
    },
    /// Answer call
    #[effect(kind = "outside")]
    #[effect(own_device, attended, icon = "phone")]
    AnswerCall {
        #[param("The call's banner; the ringing call when left out")]
        banner: Option<String>,
    },
    /// Decline call
    #[effect(kind = "outside")]
    #[effect(own_device)]
    DeclineCall {
        #[param("The call's banner; the ringing call when left out")]
        banner: Option<String>,
    },
}

/// A banner on screen, as a row of a declared type: what an agent reads to
/// find the notification to act on, and opens through
/// [`BannerEffect::Open`].
#[derive(ui::tilo_resource::Row)]
#[row(kind = "notifications.banner", label = "Notification banner", effects = BannerEffect)]
pub struct BannerRow;

/// The screenshot scenarios ([`crate::fixture`]).
pub(crate) const SCENARIOS: &[&str] = &[
    "single", "banners", "choices", "call", "more", "locked", "focused", "machine", "bars", "empty",
];

/// The banners app: the banner surface over [`Banners`].
pub struct NotificationBanners;

impl ResourceApp for NotificationBanners {
    type Resource = Banners;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = "Notifications";
    const ICON: Option<&'static str> = Some("bell");
    // Over the desktop: what is not a banner shows the desktop under it.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is nothing to look back on.
    const DOCUMENT_HISTORY: bool = false;
    // The banners show this device's desktop: they are nobody's workspace
    // member.
    const ATTACHES_RUNTIME: bool = false;

    fn view(cx: &mut ViewCx<Banners>) -> Element {
        let lent = cx.take_lent::<BannerService>();
        let surface = BannerSurface::new(lent.map(|service| service.link));
        surface.link().wake_with(cx.waker());
        cx.surface(surface)
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        crate::fixture::viewport(scenario)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_banner_action_is_published_by_its_name() {
        let ids: Vec<&str> = [
            BannerEffect::Open { banner: None },
            BannerEffect::Dismiss { banner: None },
            BannerEffect::Act {
                banner: None,
                choice: String::new(),
            },
            BannerEffect::Reply {
                banner: None,
                text: String::new(),
            },
            BannerEffect::Snooze { banner: None },
            BannerEffect::FocusNewest,
            BannerEffect::Read { banner: None },
            BannerEffect::AnswerCall { banner: None },
            BannerEffect::DeclineCall { banner: None },
        ]
        .iter()
        .map(Effect::id)
        .collect();
        assert_eq!(
            ids,
            [
                "open",
                "dismiss",
                "act",
                "reply",
                "snooze",
                "focus-newest",
                "read",
                "answer-call",
                "decline-call"
            ]
        );
    }
}

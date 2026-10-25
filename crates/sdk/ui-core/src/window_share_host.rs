//! What a desktop does for windows shared live ([`super`]), the same in the
//! browser desktop and the session's shell.
//!
//! - **The windows' menus.** Every window on a record gets "Share to
//!   channel…" in its menu, or "Stop sharing in #x" once the sharing app
//!   shares it ([`offer`]); what the person chose becomes an ask for the
//!   sharing app ([`asks`]). A share ask brings the sharing app forward.
//! - **The live views.** Each resource a window embeds runs as its own app on
//!   the desktop's own copy of the record, started once, tried again every
//!   [`RETRY`] while it cannot show, and dropped once nothing embeds it
//!   ([`Views`]). Each is laid out at about a window's size ([`VIEW`]) and
//!   drawn scaled into the place its window gave it ([`view_transform`]),
//!   clipped to what of it may show ([`placed`]). It takes no input: opening
//!   the resource is the embedding app's own action.
//!
//! What runs a view differs: the browser runs the app in its own page
//! ([`crate::widget::TileHost`]), the shell runs the app's own process on a
//! window of its own that it never shows as one. Both are a [`LiveView`].

use std::collections::{HashMap, HashSet};

use vello::kurbo::{Affine, Rect, Vec2};

use super::{Ask, Embed, EmbedState, OpenWindow, ResourceView, SharedWindow};
use crate::time::{Duration, Instant};
use crate::window_slots::{TabId, WindowSlots};
use crate::window_toolbar::ToolbarAction;

/// The size an embedded view is laid out at before it is scaled into its
/// place: about a window's, so the app lays it out as its sharer sees it.
pub const VIEW: (f64, f64) = (1000.0, 625.0);
/// How soon a view that could not show yet (its record still on its way)
/// is tried again.
pub const RETRY: Duration = Duration::from_secs(2);

/// The window menu's item that asks the sharing app to share the window.
pub const SHARE_ITEM: &str = "tilo.desktop.share-to-channel";
/// The window menu's item that asks the sharing app to stop sharing it.
pub const STOP_ITEM: &str = "tilo.desktop.stop-sharing";

/// The share the sharing app keeps of `window`, if it shares it.
fn share_of<'a>(window: &OpenWindow, shared: &'a [SharedWindow]) -> Option<&'a SharedWindow> {
    shared
        .iter()
        .find(|share| share.app == window.app && share.record == window.record)
}

/// The item `window`'s menu offers: "Stop sharing in #x" while `shared`
/// holds it, else "Share to channel…".
#[must_use]
pub fn menu_item(window: &OpenWindow, shared: &[SharedWindow]) -> ToolbarAction {
    match share_of(window, shared) {
        Some(share) => ToolbarAction::new(
            STOP_ITEM,
            "eye-off",
            crate::i18n::I18nManager::format(
                "window-stop-sharing-in",
                &[("place", (&share.place).into())],
            ),
        ),
        None => ToolbarAction::new(
            SHARE_ITEM,
            "screen-share",
            crate::i18n::I18nManager::lookup_plain("window-share-to-channel"),
        ),
    }
}

/// Put each of `windows`' share item in its menu, and take it out of the
/// menus of the windows in `offered` (the ones it stood in) that are no
/// longer among them. `offered` becomes the windows it stands in now.
pub fn offer<S: std::hash::BuildHasher + Default>(
    slots: &mut WindowSlots,
    windows: &[OpenWindow],
    shared: &[SharedWindow],
    offered: &mut HashSet<TabId, S>,
) {
    let now: HashSet<TabId, S> = windows.iter().map(|window| window.tab).collect();
    for window in windows {
        slots.set_host_items(window.tab, vec![menu_item(window, shared)]);
    }
    for gone in offered.difference(&now) {
        slots.set_host_items(*gone, Vec::new());
    }
    *offered = now;
}

/// What the person's `chosen` items of the desktop's own (by tab, as
/// [`WindowSlots::take_host_actions`] hands them) ask of the sharing app,
/// oldest first. A [`Ask::Share`] brings the sharing app forward, to ask
/// where.
#[must_use]
pub fn asks(
    chosen: Vec<(TabId, String)>,
    windows: &[OpenWindow],
    shared: &[SharedWindow],
) -> Vec<Ask> {
    let mut asks = Vec::new();
    for (tab, item) in chosen {
        let Some(window) = windows.iter().find(|window| window.tab == tab) else {
            continue;
        };
        if item == SHARE_ITEM {
            asks.push(Ask::Share(window.clone()));
        } else if item == STOP_ITEM {
            asks.extend(
                shared
                    .iter()
                    .filter(|share| share.app == window.app && share.record == window.record)
                    .map(|share| Ask::Stop {
                        message: share.message.clone(),
                    }),
            );
        }
    }
    asks
}

/// Where `embed`, declared in its window's layout coordinates, is drawn on
/// a desktop that shows the window's content at `origin` within `content`:
/// its place, and what of it may show.
#[must_use]
pub fn placed(embed: &Embed, origin: Vec2, content: Rect) -> (Rect, Rect) {
    (
        embed.rect + origin,
        (embed.clip + origin).intersect(content),
    )
}

/// The transform that draws a view laid out at [`VIEW`]'s size from the
/// origin into `rect`, through `transform`: as large as fits, keeping its
/// proportions, from `rect`'s corner.
#[must_use]
pub fn view_transform(transform: Affine, rect: Rect) -> Affine {
    let scale = (rect.width() / VIEW.0).min(rect.height() / VIEW.1);
    transform
        .pre_translate(Vec2::new(rect.x0, rect.y0))
        .pre_scale(scale)
}

/// What runs one embedded view.
pub trait LiveView {
    /// How it is coming along.
    fn state(&self) -> EmbedState;

    /// Whether it is started again once [`RETRY`] passed while it does not
    /// show: a view whose record or code was still on its way shows once it
    /// is started again.
    fn retries(&self) -> bool {
        !matches!(self.state(), EmbedState::Showing)
    }
}

impl LiveView for crate::widget::TileHost {
    fn state(&self) -> EmbedState {
        match self.content() {
            crate::widget::TileContent::App(_) => EmbedState::Showing,
            crate::widget::TileContent::Notice { detail, .. } => {
                EmbedState::Unavailable(detail.clone())
            }
            _ => EmbedState::Loading,
        }
    }
}

/// One embedded view: what runs it, or why it cannot show.
struct View<H> {
    host: Result<H, String>,
    tried: Instant,
}

impl<H: LiveView> View<H> {
    fn state(&self) -> EmbedState {
        match &self.host {
            Err(reason) => EmbedState::Unavailable(reason.clone()),
            Ok(host) => host.state(),
        }
    }

    /// Whether it waits for nothing: it shows, or what runs it says it is
    /// not started again.
    fn settled(&self) -> bool {
        match &self.host {
            Err(_) => false,
            Ok(host) => !host.retries(),
        }
    }

    /// When it is tried again, while it does not show.
    fn retry_at(&self) -> Option<Instant> {
        self.tried.checked_add(RETRY)
    }
}

/// The views a desktop runs for the windows that embed them, by the view.
pub struct Views<H> {
    views: HashMap<ResourceView, View<H>>,
}

impl<H> Default for Views<H> {
    fn default() -> Self {
        Self {
            views: HashMap::new(),
        }
    }
}

/// What [`Views::follow`] changed.
pub struct Followed<H> {
    /// A view started, was tried again or was dropped.
    pub changed: bool,
    /// What ran the views that were dropped or tried again, for the desktop
    /// to end.
    pub dropped: Vec<H>,
}

impl<H: LiveView> Views<H> {
    /// Run exactly the `wanted` views at `now`: drop the ones no longer
    /// wanted, start the ones newly wanted with `start`, and start again
    /// each one that has not shown for [`RETRY`].
    pub fn follow<'a>(
        &mut self,
        wanted: impl IntoIterator<Item = &'a ResourceView>,
        now: Instant,
        mut start: impl FnMut(&ResourceView) -> Result<H, String>,
    ) -> Followed<H> {
        let wanted: HashSet<&ResourceView> = wanted.into_iter().collect();
        let mut dropped = Vec::new();
        let gone: Vec<ResourceView> = self
            .views
            .keys()
            .filter(|view| !wanted.contains(view))
            .cloned()
            .collect();
        let mut changed = !gone.is_empty();
        for view in gone {
            if let Some(View { host: Ok(host), .. }) = self.views.remove(&view) {
                dropped.push(host);
            }
        }
        for view in wanted {
            let retry = self.views.get(view).is_none_or(|known| {
                !known.settled() && known.retry_at().is_some_and(|at| now >= at)
            });
            if retry {
                let host = start(view);
                if let Some(View { host: Ok(old), .. }) =
                    self.views.insert(view.clone(), View { host, tried: now })
                {
                    dropped.push(old);
                }
                changed = true;
            }
        }
        Followed { changed, dropped }
    }

    /// How `view` is coming along, while it is run.
    #[must_use]
    pub fn state(&self, view: &ResourceView) -> Option<EmbedState> {
        self.views.get(view).map(View::state)
    }

    /// The soonest a view that could not show is tried again.
    #[must_use]
    pub fn retry_deadline(&self) -> Option<Instant> {
        self.views
            .values()
            .filter(|view| !view.settled())
            .filter_map(View::retry_at)
            .min()
    }

    /// What runs `view`, while it runs.
    pub fn get_mut(&mut self, view: &ResourceView) -> Option<&mut H> {
        self.views
            .get_mut(view)
            .and_then(|view| view.host.as_mut().ok())
    }

    /// Every running view.
    pub fn running(&self) -> impl Iterator<Item = (&ResourceView, &H)> {
        self.views
            .iter()
            .filter_map(|(view, known)| known.host.as_ref().ok().map(|host| (view, host)))
    }

    /// Every running view, to drive.
    pub fn running_mut(&mut self) -> impl Iterator<Item = (&ResourceView, &mut H)> {
        self.views
            .iter_mut()
            .filter_map(|(view, known)| known.host.as_mut().ok().map(|host| (view, host)))
    }

    /// Whether no view runs or waits.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(EmbedState);

    impl LiveView for Fake {
        fn state(&self) -> EmbedState {
            self.0.clone()
        }
    }

    fn view(record: &str) -> ResourceView {
        ResourceView {
            app: "so.tilo.notes".into(),
            record: record.into(),
            resource: record.into(),
        }
    }

    fn window(tab: TabId, record: &str) -> OpenWindow {
        OpenWindow {
            tab,
            app: "so.tilo.notes".into(),
            app_title: "Notes".into(),
            title: "Plan".into(),
            record: record.into(),
            kind: "notes.workspace".into(),
        }
    }

    #[test]
    fn a_view_starts_once_is_retried_while_it_cannot_show_and_ends_when_unwanted() {
        let mut views: Views<Fake> = Views::default();
        let start = crate::time::now();
        let mut started = 0;
        let followed = views.follow([&view("plan")], start, |_| {
            started += 1;
            Ok(Fake(EmbedState::Loading))
        });
        assert!(followed.changed && followed.dropped.is_empty());
        assert_eq!(views.state(&view("plan")), Some(EmbedState::Loading));
        let soon = start.checked_add(Duration::from_millis(500)).expect("time");
        assert!(
            !views
                .follow([&view("plan")], soon, |_| unreachable!())
                .changed
        );
        assert_eq!(views.retry_deadline(), start.checked_add(RETRY));

        let later = start.checked_add(RETRY).expect("time");
        let followed = views.follow([&view("plan")], later, |_| {
            started += 1;
            Ok(Fake(EmbedState::Showing))
        });
        assert_eq!(followed.dropped.len(), 1, "the earlier try ends");
        assert_eq!(started, 2);
        assert_eq!(
            views.retry_deadline(),
            None,
            "a showing view waits for nothing"
        );
        let much_later = later.checked_add(RETRY * 3).expect("time");
        assert!(
            !views
                .follow([&view("plan")], much_later, |_| unreachable!())
                .changed
        );

        let followed = views.follow([], much_later, |_| unreachable!());
        assert!(followed.changed);
        assert_eq!(followed.dropped.len(), 1);
        assert!(views.is_empty());
    }

    #[test]
    fn a_view_that_cannot_start_says_why() {
        let mut views: Views<Fake> = Views::default();
        let _ = views.follow([&view("gone")], crate::time::now(), |_| {
            Err("It shows once it reaches you".into())
        });
        assert_eq!(
            views.state(&view("gone")),
            Some(EmbedState::Unavailable(
                "It shows once it reaches you".into()
            ))
        );
        assert_eq!(views.running().count(), 0);
    }

    #[test]
    fn the_menus_offer_sharing_until_shared_and_choices_become_asks() {
        let mut slots = WindowSlots::default();
        let mut offered = HashSet::new();
        let windows = vec![window(1, "plan"), window(2, "notes")];
        let shared = vec![SharedWindow {
            message: "m1".into(),
            app: "so.tilo.notes".into(),
            record: "plan".into(),
            place: "#general".into(),
        }];
        assert_eq!(menu_item(&windows[0], &shared).id, STOP_ITEM);
        assert_eq!(menu_item(&windows[1], &shared).id, SHARE_ITEM);
        offer(&mut slots, &windows, &shared, &mut offered);
        assert_eq!(offered, HashSet::from([1, 2]));
        offer(&mut slots, &windows[1..], &shared, &mut offered);
        assert_eq!(
            offered,
            HashSet::from([2]),
            "a closed window loses its item"
        );

        let asked = asks(
            vec![
                (1, STOP_ITEM.into()),
                (2, SHARE_ITEM.into()),
                (9, SHARE_ITEM.into()),
            ],
            &windows,
            &shared,
        );
        assert_eq!(
            asked,
            vec![
                Ask::Stop {
                    message: "m1".into()
                },
                Ask::Share(windows[1].clone())
            ],
            "a window no longer listed asks nothing"
        );
    }

    #[test]
    fn an_embed_lands_where_its_window_shows_and_scales_into_its_place() {
        let embed = Embed {
            key: "m1".into(),
            view: view("plan"),
            rect: Rect::new(10.0, 20.0, 510.0, 332.5),
            clip: Rect::new(0.0, 0.0, 400.0, 300.0),
        };
        let (rect, clip) = placed(
            &embed,
            Vec2::new(100.0, 50.0),
            Rect::new(100.0, 50.0, 400.0, 400.0),
        );
        assert_eq!(rect, Rect::new(110.0, 70.0, 610.0, 382.5));
        assert_eq!(clip, Rect::new(100.0, 50.0, 400.0, 350.0));
        let transform = view_transform(Affine::IDENTITY, rect);
        assert_eq!(
            transform * vello::kurbo::Point::new(VIEW.0, VIEW.1),
            vello::kurbo::Point::new(610.0, 382.5)
        );
    }
}

//! A window's page on the workspace's live plane.
//!
//! A web page is foreign content, so it is the one kind of window whose
//! drawing leaves the device: an app that runs a page engine offers its
//! window's page as a paint stream (`FloatingSurfaceClientMessage::PaintCapable`,
//! `PaintStream`), and this publishes it as one `tilo_page/1` track per window
//! on the workspace's live plane, where the members' desks draw it.
//!
//! The track exists only while someone shows it: a member's desk asks the
//! plane for `page/<tab>`, the plane tells this session
//! ([`Plane::wanted_pages`]), and only then is the app asked for a whole frame
//! ([`FloatingSurfaceHostMessage::PaintKeyframe`]). When the last viewer stops,
//! the app is told so (`PaintStop`) and nothing more is captured or sent. A
//! viewer that joins late, or whose patches stopped applying, gets a whole
//! frame first: the plane names the track in
//! [`Plane::take_page_keyframes`].
//!
//! An ask is not an answer. An app that was asked for a whole frame and handed none over within
//! [`HAND_OVER_BOUND`] is asked again, every bound, and until a whole frame comes its window is
//! among [`PageTracks::withheld`]: the shares' reading says of it that its app has not handed its
//! page over, rather than that the page is on its way.
//!
//! A page is carried whole or not at all. Its app hands a payload over in as many parts as its
//! connection needs, and the session joins them ([`crate::app_tabs`]); what the plane carries as
//! one object ([`MAX_PAYLOAD`]) is the bound. A whole frame past it reaches nobody, and its
//! window is among [`PageTracks::oversized`]: the reading says the page is too large to send,
//! and the app is asked again every bound, since a page changes.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use tilo_desktop_model::TabId;
use tilo_live::page::PageObject;
use tilo_live::plane::Plane;

/// How many payloads one page may hold for a plane that has not taken them
/// yet: past it the app is asked for a whole frame instead, which is smaller
/// than the backlog it would have to keep in order.
const MAX_QUEUED: usize = 64;
/// Largest page payload the live plane carries as one object: what its profile refuses past,
/// less what sealing adds, so that a payload this session takes is one its plane sends.
pub(crate) const MAX_PAYLOAD: usize =
    tilo_live::page::MAX_PAYLOAD_BYTES - tilo_live::frame::OVERHEAD;

/// How long an app has to hand over the whole frame it was asked for, before this session says
/// it has not and asks again. An app answers with one capture of its page and the frames it
/// draws around it; its engine gives a capture up after three seconds and captures again
/// (`ui::webview::paint`), so ten seconds are three captures that came to nothing, with the frames
/// a machine without a GPU draws around them. Longer leaves a person watching an arriving card
/// with no reason said; shorter says it of a page that is merely slow.
pub(crate) const HAND_OVER_BOUND: std::time::Duration =
    std::time::Duration::from_micros(HAND_OVER_BOUND_US);
const HAND_OVER_BOUND_US: u64 = 10_000_000;

/// What the app of a page must be told about how its window is wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Watch {
    /// Someone shows the window, and the whole frame is what starts them:
    /// `FloatingSurfaceHostMessage::PaintKeyframe`.
    Whole,
    /// Nobody shows the window any more:
    /// `FloatingSurfaceHostMessage::PaintStop`.
    Stop,
}

/// How an app stands with the whole frame of its page it was asked for, on the clock
/// [`PageTracks::step`] is told.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Asked {
    /// It owes none: it was asked for none, or handed over the one it was asked for.
    #[default]
    Nothing,
    /// It was asked at this time, and has [`HAND_OVER_BOUND`] from then.
    Since(u64),
    /// It let [`HAND_OVER_BOUND`] pass without handing one over, and was last asked again at
    /// this time.
    Overdue(u64),
}

/// One window whose app offers its page.
#[derive(Default)]
struct Offered {
    /// The workspace whose plane carries the page.
    workspace: String,
    /// Whether the app was told someone shows the window.
    watched: bool,
    /// Whether the app was asked for a whole frame that has not arrived yet:
    /// the changes before it would apply to a frame nobody holds.
    awaiting_whole: bool,
    /// What the app sent and the plane has not taken yet, oldest first: each
    /// payload with whether it is a whole frame.
    queue: VecDeque<(bool, Vec<u8>)>,
    /// The number of objects this window's page has published.
    sequence: u64,
    /// Whether a whole frame of the page was published since someone started showing the
    /// window: until then nobody was sent anything they can draw.
    sent: bool,
    /// How the app stands with the whole frame it was asked for and has not handed over since.
    asked: Asked,
    /// The size of the whole frame the app has and the plane carries no object for, until a
    /// whole frame it does carry comes.
    too_large: Option<usize>,
}

impl Offered {
    /// What the app must be told now of a window a member's desk shows (`shown`) or not, whose
    /// viewers must start over at a whole frame (`start_over`: one joined, or lost the chain of
    /// changes). A window that waits for a whole frame is asked for it once, and again each time
    /// [`HAND_OVER_BOUND`] passes without one: also a window that waits because the plane fell
    /// behind, which nobody else asks.
    fn follow(&mut self, shown: bool, start_over: bool, now_us: u64) -> Option<Watch> {
        if !shown {
            if !self.watched {
                return None;
            }
            self.watched = false;
            self.awaiting_whole = false;
            self.sent = false;
            self.asked = Asked::Nothing;
            self.too_large = None;
            self.queue.clear();
            return Some(Watch::Stop);
        }
        if !self.watched {
            self.watched = true;
            self.awaiting_whole = true;
            self.sent = false;
            self.asked = Asked::Nothing;
            self.too_large = None;
            self.queue.clear();
        } else if start_over && !self.awaiting_whole {
            // A viewer joined, or lost the chain: the next frame starts
            // everyone over, a still page included.
            self.awaiting_whole = true;
            self.asked = Asked::Nothing;
            self.queue.clear();
        }
        if !self.awaiting_whole {
            return None;
        }
        self.asked = match self.asked {
            Asked::Nothing => Asked::Since(now_us),
            Asked::Since(at) | Asked::Overdue(at)
                if now_us.saturating_sub(at) < HAND_OVER_BOUND_US =>
            {
                return None;
            }
            Asked::Since(_) | Asked::Overdue(_) => Asked::Overdue(now_us),
        };
        Some(Watch::Whole)
    }
}

/// The windows whose pages this session publishes, by the tab that shows them.
#[derive(Default)]
pub(crate) struct PageTracks {
    offered: BTreeMap<TabId, Offered>,
}

impl PageTracks {
    pub(crate) fn new() -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self::default()))
    }

    /// The window `tab` of `workspace` runs a page engine: it can hand over
    /// its page when someone shows it.
    pub(crate) fn capable(&mut self, tab: TabId, workspace: &str) {
        let page = self.offered.entry(tab).or_default();
        workspace.clone_into(&mut page.workspace);
    }

    /// Whether the window `tab` hands over the page it draws to `workspace`'s
    /// members: its app says it streams one there.
    pub(crate) fn streamed(&self, tab: TabId, workspace: &str) -> bool {
        self.offered
            .get(&tab)
            .is_some_and(|page| page.workspace == workspace)
    }

    /// The window `tab` closed, or its app went: nothing of its page is
    /// published from here on.
    pub(crate) fn closed(&mut self, tab: TabId) {
        self.offered.remove(&tab);
    }

    /// One payload the app handed over, as it encoded it. A payload nobody shows is dropped;
    /// one larger than the plane carries is not sent, and said ([`Self::too_large`]).
    pub(crate) fn received(&mut self, tab: TabId, whole: bool, payload: Vec<u8>) {
        if payload.len() > MAX_PAYLOAD {
            self.too_large(tab, whole, payload.len());
            return;
        }
        let Some(page) = self.offered.get_mut(&tab) else {
            return;
        };
        if !page.watched {
            return;
        }
        if page.awaiting_whole {
            if !whole {
                return;
            }
            page.awaiting_whole = false;
            page.asked = Asked::Nothing;
            page.queue.clear();
        }
        if whole {
            page.too_large = None;
        }
        if page.queue.len() >= MAX_QUEUED {
            // The plane is behind: a whole frame is what catches it up, and the next pass asks
            // the app for it.
            page.queue.clear();
            page.awaiting_whole = true;
            page.asked = Asked::Nothing;
            return;
        }
        page.queue.push_back((whole, payload));
    }

    /// The app of the window `tab` has a payload of `bytes` bytes, which the plane carries no
    /// object for ([`MAX_PAYLOAD`]). Nothing the app sends after it applies to what the members
    /// showing the window hold, so they start over at a whole frame, which the app is asked
    /// for. A whole frame that large means the page as it stands reaches nobody: the window is
    /// among [`Self::oversized`] until a whole frame the plane carries comes.
    pub(crate) fn too_large(&mut self, tab: TabId, whole: bool, bytes: usize) {
        let Some(page) = self.offered.get_mut(&tab) else {
            return;
        };
        if !page.watched {
            return;
        }
        page.queue.clear();
        if whole {
            page.too_large = Some(bytes);
            // Whatever went out before is not this page.
            page.sent = false;
        }
        if !page.awaiting_whole {
            page.awaiting_whole = true;
            page.asked = Asked::Nothing;
        }
    }

    /// What the app of the window `tab` handed over broke off: a payload it sent in parts did
    /// not come whole. The members showing the window start over at a whole frame, which the
    /// app is asked for.
    pub(crate) fn lost(&mut self, tab: TabId) {
        let Some(page) = self.offered.get_mut(&tab) else {
            return;
        };
        if page.watched && !page.awaiting_whole {
            page.queue.clear();
            page.awaiting_whole = true;
            page.asked = Asked::Nothing;
        }
    }

    /// What the planes ask of the pages this session offers, and what the apps
    /// sent: every window someone shows gets a keyframe asked of it, every
    /// window nobody shows any more is told to stop, and the payloads the
    /// planes want are published. The asks go back to the apps; an app that
    /// answers none within [`HAND_OVER_BOUND`] is asked again. `now_us` is a
    /// clock in microseconds.
    ///
    /// `shared` are the windows this session shares as their page. A plane takes any member's
    /// ask for any window's page track, so what a desk asks for is no word on whether it may
    /// have it: a window that is not shared is shown to nobody, whoever asks.
    pub(crate) fn step(
        &mut self,
        planes: &BTreeMap<String, Arc<Plane>>,
        shared: &BTreeSet<TabId>,
        now_us: u64,
    ) -> Vec<(TabId, Watch)> {
        let mut wanted: BTreeSet<(String, TabId)> = BTreeSet::new();
        let mut wholes: BTreeSet<(String, TabId)> = BTreeSet::new();
        for (workspace, plane) in planes {
            for tab in plane.wanted_pages() {
                wanted.insert((workspace.clone(), tab));
            }
            for tab in plane.take_page_keyframes() {
                wholes.insert((workspace.clone(), tab));
            }
        }
        let mut asks = Vec::new();
        for (tab, page) in &mut self.offered {
            let key = (page.workspace.clone(), *tab);
            let shown = shared.contains(tab) && wanted.contains(&key);
            let ask = page.follow(shown, wholes.contains(&key), now_us);
            if let Some(watch) = ask {
                asks.push((*tab, watch));
            }
            if ask == Some(Watch::Stop) {
                continue;
            }
            let Some(plane) = planes.get(&page.workspace) else {
                page.queue.clear();
                continue;
            };
            for (whole, payload) in std::mem::take(&mut page.queue) {
                let object = PageObject {
                    whole,
                    sequence: page.sequence,
                    timestamp_us: now_us,
                    payload,
                };
                if plane.send_page(*tab, &object).is_ok() {
                    page.sequence = page.sequence.wrapping_add(1);
                    page.sent |= object.whole;
                }
            }
        }
        asks
    }

    /// The windows whose page somebody shows: a member's desk wants the track, so the page is in
    /// use while it does, whether or not an action runs on it.
    pub(crate) fn viewed(&self) -> Vec<TabId> {
        self.offered
            .iter()
            .filter(|(_, page)| page.watched)
            .map(|(tab, _)| *tab)
            .collect()
    }

    /// The windows whose page went out to the members showing them: a whole frame of it was
    /// published since a desk asked for it. A window that is asked for and whose app handed
    /// nothing over is not among them.
    pub(crate) fn sent(&self) -> Vec<TabId> {
        self.offered
            .iter()
            .filter(|(_, page)| page.watched && page.sent)
            .map(|(tab, _)| *tab)
            .collect()
    }

    /// The windows a member shows whose app was asked for a whole frame of its page and let
    /// [`HAND_OVER_BOUND`] pass without handing one over: nothing new of them goes out, and it is
    /// the app that keeps them waiting.
    pub(crate) fn withheld(&self) -> Vec<TabId> {
        self.offered
            .iter()
            .filter(|(_, page)| {
                page.watched && page.too_large.is_none() && matches!(page.asked, Asked::Overdue(_))
            })
            .map(|(tab, _)| *tab)
            .collect()
    }

    /// The windows a member shows whose page, as it stands, is larger than the plane carries
    /// ([`MAX_PAYLOAD`]): its app has it, and nobody can be sent it.
    pub(crate) fn oversized(&self) -> Vec<TabId> {
        self.offered
            .iter()
            .filter(|(_, page)| page.watched && page.too_large.is_some())
            .map(|(tab, _)| *tab)
            .collect()
    }

    /// How long from `now_us` until the next app that was asked for a whole frame has let
    /// [`HAND_OVER_BOUND`] pass: when the session next has something to say, and to ask, with no
    /// message to wake it.
    pub(crate) fn due_in(&self, now_us: u64) -> Option<std::time::Duration> {
        self.offered
            .values()
            .filter(|page| page.watched && page.awaiting_whole)
            .filter_map(|page| match page.asked {
                Asked::Nothing => None,
                Asked::Since(at) | Asked::Overdue(at) => Some(at),
            })
            .map(|asked| {
                asked
                    .saturating_add(HAND_OVER_BOUND_US)
                    .saturating_sub(now_us)
            })
            .min()
            .map(std::time::Duration::from_micros)
    }

    /// The windows this session publishes a page of: those whose app said it
    /// streams one.
    #[cfg(test)]
    pub(crate) fn offered(&self) -> Vec<TabId> {
        self.offered.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(workspace: &str) -> PageTracks {
        let mut pages = PageTracks::default();
        pages.capable(7, workspace);
        pages
    }

    #[test]
    fn a_page_capable_window_is_offered_and_goes_with_its_tab() {
        let mut pages = page("w1");
        assert_eq!(pages.offered(), [7]);
        pages.closed(7);
        assert!(pages.offered().is_empty());
    }

    #[test]
    fn nothing_is_kept_of_a_page_nobody_shows() {
        let mut pages = page("w1");
        pages.received(7, true, vec![0, 1, 2]);
        pages.received(7, false, vec![1, 2, 3]);
        let page = pages.offered.get(&7).unwrap();
        assert!(page.queue.is_empty(), "no plane asked for it");
        assert!(!page.watched);
    }

    #[test]
    fn a_watched_page_keeps_its_payloads_in_order() {
        let mut pages = page("w1");
        let page = pages.offered.get_mut(&7).unwrap();
        page.watched = true;
        pages.received(7, true, vec![0, 1, 2]);
        pages.received(7, false, vec![3]);
        let page = pages.offered.get(&7).unwrap();
        assert_eq!(
            page.queue.iter().cloned().collect::<Vec<_>>(),
            [(true, vec![0, 1, 2]), (false, vec![3])]
        );
    }

    #[test]
    fn a_change_that_arrives_before_the_whole_frame_is_dropped() {
        let mut pages = page("w1");
        let page = pages.offered.get_mut(&7).unwrap();
        page.watched = true;
        page.awaiting_whole = true;
        pages.received(7, false, vec![3]);
        assert!(pages.offered.get(&7).unwrap().queue.is_empty());
        pages.received(7, true, vec![0, 1, 2]);
        let page = pages.offered.get(&7).unwrap();
        assert!(!page.awaiting_whole);
        assert_eq!(
            page.queue.iter().cloned().collect::<Vec<_>>(),
            [(true, vec![0, 1, 2])]
        );
    }

    #[test]
    fn a_backlog_past_the_bound_starts_over_at_a_whole_frame() {
        let mut pages = page("w1");
        {
            let page = pages.offered.get_mut(&7).unwrap();
            page.watched = true;
        }
        for _ in 0..MAX_QUEUED {
            pages.received(7, false, vec![1]);
        }
        pages.received(7, false, vec![1]);
        let page = pages.offered.get_mut(&7).unwrap();
        assert!(page.queue.is_empty());
        assert!(page.awaiting_whole, "a whole frame catches up the plane");
        assert_eq!(
            page.follow(true, false, 5),
            Some(Watch::Whole),
            "and the app is asked for the frame the session waits for"
        );
    }

    /// An app that is asked for a whole frame and hands none over is asked again once the bound
    /// passed, and from then its window is said to be withheld by it; the whole frame it then
    /// hands over ends both.
    #[test]
    fn an_app_that_hands_nothing_over_is_asked_again_and_said_to_withhold_its_page() {
        let mut pages = page("w1");
        let asked = |pages: &mut PageTracks, now_us: u64| {
            pages
                .offered
                .get_mut(&7)
                .unwrap()
                .follow(true, false, now_us)
        };
        assert_eq!(asked(&mut pages, 1_000), Some(Watch::Whole));
        assert_eq!(
            pages.due_in(1_000),
            Some(HAND_OVER_BOUND),
            "the session comes back for it when the bound passes"
        );
        assert_eq!(asked(&mut pages, 1_000 + HAND_OVER_BOUND_US - 1), None);
        assert!(pages.withheld().is_empty(), "the app still has time");
        // A change is no answer to an ask for a whole frame.
        pages.received(7, false, vec![3]);
        assert_eq!(
            asked(&mut pages, 1_000 + HAND_OVER_BOUND_US),
            Some(Watch::Whole),
            "the bound passed: the app is asked again"
        );
        assert_eq!(pages.withheld(), [7]);
        assert!(pages.sent().is_empty());
        assert_eq!(asked(&mut pages, 1_001 + HAND_OVER_BOUND_US), None);
        assert_eq!(
            asked(&mut pages, 1_000 + 2 * HAND_OVER_BOUND_US),
            Some(Watch::Whole),
            "and again a bound later"
        );
        pages.received(7, true, vec![0, 1, 2]);
        assert!(pages.withheld().is_empty(), "the app handed its page over");
        assert_eq!(pages.due_in(1_000 + 2 * HAND_OVER_BOUND_US), None);
        assert_eq!(asked(&mut pages, 1_000 + 3 * HAND_OVER_BOUND_US), None);
        // Nobody shows the window any more: nothing is held against the app.
        let page = pages.offered.get_mut(&7).unwrap();
        page.awaiting_whole = true;
        page.asked = Asked::Overdue(0);
        assert_eq!(pages.withheld(), [7]);
        assert_eq!(
            asked_stop(&mut pages),
            Some(Watch::Stop),
            "the app is told nobody shows the window"
        );
        assert!(pages.withheld().is_empty());
    }

    fn asked_stop(pages: &mut PageTracks) -> Option<Watch> {
        pages.offered.get_mut(&7).unwrap().follow(false, false, 0)
    }

    /// A whole frame larger than the plane carries goes to nobody and is said to be too large,
    /// in other words than a page its app withholds; the app is asked again a bound later, and
    /// the whole frame the plane does carry ends it. A change too large only starts the viewers
    /// over.
    #[test]
    fn a_whole_frame_larger_than_the_plane_carries_is_said_and_asked_for_again() {
        let mut pages = page("w1");
        let asked = |pages: &mut PageTracks, now_us: u64| {
            pages
                .offered
                .get_mut(&7)
                .unwrap()
                .follow(true, false, now_us)
        };
        assert_eq!(asked(&mut pages, 1_000), Some(Watch::Whole));
        pages.received(7, true, vec![0; MAX_PAYLOAD + 1]);
        assert_eq!(pages.oversized(), [7]);
        assert!(pages.sent().is_empty(), "nobody was sent the page");
        assert!(pages.offered.get(&7).unwrap().queue.is_empty());
        assert_eq!(asked(&mut pages, 2_000), None, "the app was just asked");
        assert_eq!(
            asked(&mut pages, 1_000 + HAND_OVER_BOUND_US),
            Some(Watch::Whole),
            "a page changes: the app is asked again"
        );
        assert_eq!(pages.oversized(), [7]);
        assert!(
            pages.withheld().is_empty(),
            "the app handed over what it has: it withholds nothing"
        );
        // The same said by the first part of a payload, before the rest came.
        pages.too_large(7, true, 9 * 1024 * 1024);
        assert_eq!(pages.oversized(), [7]);
        // The page became one the plane carries.
        pages.received(7, true, vec![0; MAX_PAYLOAD]);
        assert!(pages.oversized().is_empty());
        assert_eq!(pages.offered.get(&7).unwrap().queue.len(), 1);
        // A change too large for the plane: the viewers start over at a whole frame.
        pages.received(7, false, vec![1; MAX_PAYLOAD + 1]);
        assert!(
            pages.oversized().is_empty(),
            "the page itself is not said to be too large"
        );
        assert!(pages.offered.get(&7).unwrap().queue.is_empty());
        assert_eq!(
            asked(&mut pages, 3_000 + HAND_OVER_BOUND_US),
            Some(Watch::Whole)
        );
        // A payload that broke off does the same.
        pages.received(7, true, vec![0, 1]);
        pages.lost(7);
        assert_eq!(
            asked(&mut pages, 4_000 + HAND_OVER_BOUND_US),
            Some(Watch::Whole)
        );
    }
}

/// A window's page on the workspace's live plane, as a session runs it: what
/// the app's engine captured is republished object for object, a viewer draws
/// the same frame the publisher held, and nothing is captured or sent while
/// nobody shows the window.
#[cfg(test)]
#[cfg(target_os = "linux")]
mod over_the_plane {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use channel_crypto::{ChannelGroup, MemberKey};
    use tilo_live::live::Track;
    use tilo_live::plane::{Join, Plane};
    use tilo_live::relay::{Admission, Denied, Peer, Relay, RelayAuthority};
    use tilo_live::roster::Roster as LiveRoster;
    use tilo_live::scope::Scope;
    use ui::webview::paint::stream::wire::{Payload, Receiver};

    use super::*;
    use ui::webview::paint::PaintEngine;
    use ui::webview::test_engine::TestEngine;
    use ui::webview::{WebViewCommand, WebViewEngine};

    const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
    const TAB: u64 = 0x5EED_0000_0000_0007;

    /// The engine's page: a snapshot with a header, a sidebar and a button.
    const TOP: &str = include_str!("../../../browser/paint-stream/tests/fixtures/layers.top.json");

    /// The relay the test's planes meet on: in-process, with no one to admit,
    /// since a local plane is its own relay's participant.
    fn relay() -> Arc<Relay> {
        struct Nobody;
        impl RelayAuthority for Nobody {
            fn admit<'a>(&'a self, _peer: Peer, _proof: &'a [u8]) -> Admission<'a> {
                Box::pin(std::future::ready(Err(Denied)))
            }
        }
        Arc::new(Relay::new(Arc::new(Nobody)))
    }

    /// The workspace scope's content keys, as its own channel hands them to
    /// its members.
    fn keys() -> channel_crypto::ChannelContentKeys {
        let channel = collab_types::WorkspaceId::parse(WORKSPACE).unwrap();
        ChannelGroup::create(channel, MemberKey::generate("owner").expect("a member key"))
            .expect("the workspace's channel")
            .content_keys()
            .expect("the workspace's keys")
    }

    /// `device`'s plane in the test workspace, hearing and heard by `devices`.
    fn plane(
        relay: &Relay,
        device: [u8; 32],
        devices: &[[u8; 32]],
        keys: &channel_crypto::ChannelContentKeys,
    ) -> Arc<Plane> {
        Arc::new(
            Plane::local(
                relay,
                &Join {
                    scope: Scope::workspace(WORKSPACE.to_owned()),
                    device,
                    keys: keys.clone().into(),
                    roster: LiveRoster::new(devices.iter().copied()),
                    publish: true,
                },
            )
            .expect("a local plane"),
        )
    }

    /// Drive the engine as a window does: read the frame, drain events, until
    /// its snapshot reached the paint source.
    fn settle(engine: &PaintEngine) {
        for _ in 0..4 {
            drop(engine.paint_frame());
            drop(engine.drain_events());
        }
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(20))
            .expect("a deadline");
        while !done() {
            assert!(Instant::now() < deadline, "gave up waiting: {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A desk asks for the page of a window nobody shared: its app is asked for nothing, and
    /// the window is shown to nobody.
    fn unshared_is_shown_to_nobody(pages: &mut PageTracks, planes: &BTreeMap<String, Arc<Plane>>) {
        assert!(
            pages.step(planes, &BTreeSet::new(), 0).is_empty(),
            "the page of a window nobody shared was asked of its app"
        );
        assert!(pages.viewed().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_windows_page_reaches_its_viewer_and_only_while_one_shows_it() {
        let relay = relay();
        let (a, b) = ([0xA1; 32], [0xB2; 32]);
        let devices = [a, b];
        let keys = keys();
        let publishing = plane(&relay, a, &devices, &keys);
        let watching = plane(&relay, b, &devices, &keys);
        let mut planes = BTreeMap::new();
        planes.insert(WORKSPACE.to_owned(), Arc::clone(&publishing));

        // The app's engine captures its page as the paint stream it hands the
        // session.
        let test = TestEngine::new();
        test.set_paint_snapshot(TOP);
        let engine = PaintEngine::with_interval(test, Duration::ZERO);
        engine.send(WebViewCommand::Resize {
            width: 800,
            height: 600,
            scale_factor: 1.0,
        });
        let mut pages = PageTracks::default();
        pages.capable(TAB, WORKSPACE);
        let shared = BTreeSet::from([TAB]);
        // Nobody shows the window: the session asks the app for nothing, and
        // publishes nothing.
        assert!(pages.step(&planes, &shared, 0).is_empty());
        assert!(
            publishing
                .send_page(
                    TAB,
                    &PageObject {
                        whole: true,
                        sequence: 0,
                        timestamp_us: 0,
                        payload: vec![0],
                    }
                )
                .is_err()
        );

        // A member's desk shows the window: the app is asked for a whole frame.
        watching.show(a, &[Track::Page(TAB)]);
        wait_until("the window's page is asked for", || {
            publishing.wanted_pages() == [TAB]
        });
        unshared_is_shown_to_nobody(&mut pages, &planes);
        assert_eq!(pages.step(&planes, &shared, 0), [(TAB, Watch::Whole)]);
        assert!(
            pages.sent().is_empty(),
            "a page that is asked for and not handed over went to nobody"
        );

        // The app answers with what its engine captured, and the session
        // publishes every object of it.
        settle(&engine);
        for payload in engine.take_paint_stream(true) {
            let bytes = payload.encode().expect("a payload");
            let whole = Payload::is_key(&bytes);
            pages.received(TAB, whole, bytes);
        }
        assert!(
            pages.step(&planes, &shared, 1_000).is_empty(),
            "the ask was answered"
        );
        assert_eq!(pages.sent(), [TAB], "its whole frame went out");

        // The viewer draws the page: the objects it opened rebuild the very
        // frame the publisher held.
        let mut viewer = Receiver::new();
        let mut images = 0_u32;
        wait_until("the page reaches the viewer", || {
            while let Some(heard) = watching.try_recv() {
                if heard.track != Track::Page(TAB) {
                    continue;
                }
                let object = PageObject::decode(&heard.body).expect("a page object");
                if viewer
                    .apply(Payload::decode(&object.payload).expect("a payload"))
                    .is_some()
                {
                    images = images.saturating_add(1);
                }
            }
            viewer.frame().is_some()
        });
        assert_eq!(images, 1, "the page's image came with it, by content hash");
        let published = engine.paint_frame().expect("the publisher's frame");
        assert_eq!(
            viewer.frame().expect("the viewer holds a frame"),
            &*published,
            "the viewer draws what was published"
        );

        // The desk stops showing the window: the app is told so, and nothing
        // more of it is captured or sent.
        watching.show(a, &[]);
        wait_until("the page is no longer shown", || {
            publishing.wanted_pages().is_empty()
        });
        assert_eq!(pages.step(&planes, &shared, 2_000), [(TAB, Watch::Stop)]);
        assert!(pages.sent().is_empty(), "nobody shows the window any more");
        pages.received(TAB, false, vec![0]);
        assert!(pages.step(&planes, &shared, 3_000).is_empty());
        assert!(
            publishing
                .send_page(
                    TAB,
                    &PageObject {
                        whole: false,
                        sequence: 1,
                        timestamp_us: 3_000,
                        payload: vec![0],
                    }
                )
                .is_err()
        );
    }
}

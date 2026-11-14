//! A window whose app draws its own page, shared with the workspace. Nothing of such a window is
//! captured: its app hands the session the page as a display list, the workspace's live plane
//! carries it on a track of its own, and the device showing the window draws it in the tab its
//! desk lists the window in. The test plays the app: it says its window runs a page engine, and
//! hands over what a page engine captured of a page.

use tilo_desktop_surface::{PeerRead, Standing};
use tilo_live::live::Track;
use ui::webview::paint::PaintEngine;
use ui::webview::paint::stream::wire::{CODEC, Payload};
use ui::webview::test_engine::TestEngine;
use ui::webview::{WebViewCommand, WebViewEngine};

use super::*;

/// The engine's page: a snapshot with a header, a sidebar and a button.
const TOP: &str =
    include_str!("../../../../../browser/paint-stream/tests/fixtures/layers.top.json");

/// How long one link of the page's way may take on a machine busy with the rest of the suite.
const LINK: Duration = Duration::from_secs(120);

/// How often the app hands its page over again. An app answers each ask for a whole frame; this
/// one cannot hear the ask, so it hands the whole page over while it is wanted.
const HANDED_EVERY: Duration = Duration::from_millis(250);

/// What the window's app hands the session of its page, each payload with whether it is a whole
/// frame: the whole frame, then the images it names.
fn page_objects() -> Vec<(bool, Vec<u8>)> {
    let test = TestEngine::new();
    test.set_paint_snapshot(TOP);
    let engine = PaintEngine::with_interval(test, Duration::ZERO);
    engine.send(WebViewCommand::Resize {
        width: 800,
        height: 600,
        scale_factor: 1.0,
    });
    for _ in 0..4 {
        drop(engine.paint_frame());
        drop(engine.drain_events());
    }
    engine
        .take_paint_stream(true)
        .iter()
        .map(|payload| {
            let bytes = payload.encode().expect("an encoded payload");
            (Payload::is_key(&bytes), bytes)
        })
        .collect()
}

/// Two devices of one workspace. One runs a window whose app draws its page, and shares it; the
/// other lists that window as one that runs on the first, as the workspace's layout brings it.
struct Two {
    host: Box<Member>,
    /// The client whose window the host shares.
    window: Option<TestClient>,
    viewer: Box<Member>,
    /// The tab the window stands in, on both desks.
    tab: u64,
    /// What the app hands over of its page.
    page: Vec<(bool, Vec<u8>)>,
    /// When the app last handed its page over.
    handed_at: Option<Instant>,
}

impl Two {
    /// The device of the first seed opens a window whose app says it draws a page, and shares it
    /// with the workspace; the device of the second lists the window as one that runs on the
    /// first, and reads the share from the workspace's governance.
    fn sharing(seeds: [u8; 2]) -> Self {
        tilo_window_share::flatpak_share::read_encoders();
        let salt = tilo_desktop_model::ids::device_salt_of_key(device(seeds[0]).as_bytes());
        let mut host = Box::new(member_minting_under(
            seeds[0],
            desk(
                TileContent::LocalApp {
                    app_id: APP.to_owned(),
                    label: "Notes".to_owned(),
                    note: String::new(),
                },
                tilo_desktop_model::ids::compose_id(salt, 1),
            ),
            salt,
        ));
        let mut window = Some(mapped(&mut host.session, APP));
        let tab = tab_of(&host.session, APP);
        // The window's app runs a page engine, and said so when its window opened.
        host.session
            .shared
            .pages
            .borrow_mut()
            .capable(tab, WORKSPACE);
        let mut viewer = Box::new(member_minting_under(
            seeds[1],
            showing(device(seeds[0]), APP, tab),
            1,
        ));
        share(&mut host, APP, tab);
        until(&mut host, &mut window, &mut viewer, |host, _, _| {
            !rows(&shares(&host.session), "sharing").is_empty()
        });
        carry_governance(&host.session, &viewer.session);
        heard(&viewer, seeds[0], "Framework");
        Self {
            host,
            window,
            viewer,
            tab,
            page: page_objects(),
            handed_at: None,
        }
    }

    /// One pass of the window's client and of both sessions, with what their planes said carried
    /// after it. With `hands_over`, the app hands the session its page as it stands.
    fn pass(&mut self, hands_over: bool) {
        let due = self.handed_at.is_none_or(|at| at.elapsed() >= HANDED_EVERY);
        if hands_over && due {
            let mut pages = self.host.session.shared.pages.borrow_mut();
            for (whole, bytes) in &self.page {
                pages.received(self.tab, *whole, bytes.clone());
            }
            drop(pages);
            self.handed_at = Some(Instant::now());
        }
        if let Some(client) = self.window.as_mut() {
            client.exchange();
        }
        self.host
            .session
            .step(Duration::from_millis(2))
            .expect("step");
        if let Some(client) = self.window.as_mut() {
            client.exchange();
        }
        self.viewer
            .session
            .step(Duration::from_millis(2))
            .expect("step");
        carry(&mut self.host, &self.viewer);
        carry(&mut self.viewer, &self.host);
    }

    /// Pass until `done` holds of the two, for at most `limit`: whether it came to hold.
    fn within(
        &mut self,
        limit: Duration,
        hands_over: bool,
        mut done: impl FnMut(&Self) -> bool,
    ) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            self.pass(hands_over);
            if done(self) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
        }
    }

    /// What the watching device says it received of the window.
    fn received(&self) -> Option<Value> {
        let wanted = self.tab.to_string();
        rows(&shares(&self.viewer.session), "received")
            .iter()
            .find(|stream| stream["tab"].as_str() == Some(wanted.as_str()))
            .cloned()
    }

    /// What the sharing device says of the window it shares.
    fn shared(&self) -> Option<Value> {
        let wanted = self.tab.to_string();
        rows(&shares(&self.host.session), "sharing")
            .iter()
            .find(|shared| shared["tab"].as_str() == Some(wanted.as_str()))
            .cloned()
    }

    /// What the tile of the window publishes on the watching desk, once the tab holds one.
    fn tile(&self) -> Option<PeerRead> {
        self.viewer.session.handle().peer_surface_read(self.tab)
    }

    /// The tracks the watching device asks the workspace's live plane for of the sharing device.
    fn asked(&self) -> Vec<Track> {
        self.viewer
            .session
            .shared
            .window_shares
            .borrow()
            .shown_tracks
            .get(WORKSPACE)
            .and_then(|hosts| hosts.get(&self.host.sync.publisher()))
            .map(|tracks| tracks.iter().copied().collect())
            .unwrap_or_default()
    }

    /// The windows whose page the sharing session's plane is asked for.
    fn wanted(&self) -> Vec<u64> {
        self.host.session.shared.pages.borrow().viewed()
    }

    /// How many objects the watching device's live plane opened in the workspace. It asks its
    /// plane for the one window's page and nothing else, so these are objects of that page.
    fn opened(&self) -> u64 {
        crate::calls::planes_of(&self.viewer.sync)
            .get(WORKSPACE)
            .map_or(0, |plane| plane.heard_total())
    }

    /// What both sessions say of their shares, for a failure's message.
    fn said(&self) -> String {
        format!(
            "\n  sharing device: {}\n  watching device: {}\n  its tile: {:?}",
            shares(&self.host.session),
            shares(&self.viewer.session),
            self.tile()
        )
    }
}

/// The page of a shared window is drawn on the device watching it, in the tab its desk lists the
/// window in. Every link of its way is checked in order: the sharing device announces the page,
/// the watching desk mounts its tile and asks the plane for the page's track, the app is asked for
/// the page and hands it over, the plane carries it, and the tile leaves its arriving card for the
/// page it now holds.
#[test]
fn a_shared_page_is_drawn_in_the_tab_the_watching_desk_lists_its_window_in() {
    let mut two = Two::sharing([0xc1, 0xc2]);
    let tab = two.tab;
    let host_key = hex::encode(device(0xc1).as_bytes());

    // Announced: the watching device holds the page's start, and the tab that lists the window
    // holds the tile that draws it, which has nothing to draw yet.
    assert!(
        two.within(LINK, false, |two| {
            two.received()
                .is_some_and(|stream| stream["codec"].as_str() == Some(CODEC))
                && two.tile().is_some()
        }),
        "the page was never announced to the device showing its window:{}",
        two.said()
    );
    assert_eq!(
        two.viewer.session.handle().peer_surface_tabs(),
        vec![tab],
        "the tile stands in the tab the desk lists the window in"
    );
    let arriving = two.tile().expect("the tab holds the window's tile");
    assert!(arriving.page, "the tile draws the window's page");
    assert_eq!(
        arriving.standing,
        Standing::Started,
        "nothing of the page was handed over yet"
    );

    // Asked for: the watching desk asks the workspace's live plane for the page's track, and the
    // sharing session reads that the page is wanted, which is when it asks the app for it.
    assert!(
        two.within(LINK, false, |two| {
            two.asked() == [Track::Page(tab)] && two.wanted() == [tab]
        }),
        "the page's track was never asked for: the watching device asks for {:?}, the sharing \
         device's plane is asked for the pages of {:?}:{}",
        two.asked(),
        two.wanted(),
        two.said()
    );

    // Sent, received, handed to the tile and drawn: the app hands its page over, and the tile on
    // the watching desk holds it.
    let before = two.opened();
    let drawn = two.within(LINK, true, |two| {
        two.tile()
            .is_some_and(|tile| tile.standing == Standing::Pictured)
    });
    let opened = two.opened().saturating_sub(before);
    assert!(
        opened > 0,
        "no object of the page reached the watching device's plane:{}",
        two.said()
    );
    assert!(
        drawn,
        "{opened} objects of the page reached the watching device's plane, and its tile still \
         shows the arriving card:{}",
        two.said()
    );
    assert_eq!(
        two.viewer.session.handle().peer_surface_tabs(),
        vec![tab],
        "the page is drawn in the tab the desk lists the window in"
    );
    assert_eq!(
        hosted(&two.viewer.session, tab).as_deref(),
        Some(host_key.as_str()),
        "which the desk still lists as a window that runs on its host"
    );
}

/// What the two sessions say of a shared page is what is on the two screens. While the watching
/// device holds only the page's start, it does not say the window is pictured, and says what it
/// waits for; while the app has handed over nothing, the sharing device does not say it sends the
/// window. Both say so once the page crossed, and the sharing device's plane names the window it is
/// asked for.
#[test]
fn a_page_that_only_started_is_not_said_to_be_pictured_or_sent() {
    let mut two = Two::sharing([0xc3, 0xc4]);
    let tab = two.tab.to_string();

    // The page is announced and its track asked for, and the app has handed over nothing.
    assert!(
        two.within(LINK, false, |two| {
            two.received().is_some() && two.tile().is_some() && two.wanted() == [two.tab]
        }),
        "the page was never announced and asked for:{}",
        two.said()
    );
    for _ in 0..8 {
        two.pass(false);
    }
    let received = two.received().expect("the watching device holds its start");
    assert_eq!(
        received["pictured"].as_bool(),
        Some(false),
        "a window of which only the start arrived is not pictured:{}",
        two.said()
    );
    assert!(
        received["waiting_for"]
            .as_str()
            .is_some_and(|waits| !waits.is_empty()),
        "the watching device says what the window waits for:{}",
        two.said()
    );
    let shared = two.shared().expect("the sharing device shares the window");
    assert_eq!(
        shared["streaming"].as_bool(),
        Some(false),
        "nothing of the page was sent to anyone yet:{}",
        two.said()
    );
    assert!(
        shared["waiting_for"]
            .as_str()
            .is_some_and(|waits| !waits.is_empty()),
        "the sharing device says what the window waits for:{}",
        two.said()
    );
    let reading = shares(&two.host.session);
    let plane = rows(&reading, "live")
        .iter()
        .find(|plane| plane["room"].as_str() == Some(WORKSPACE))
        .expect("the reading names the workspace's live plane");
    assert_eq!(
        plane["asked_for"].as_array().map(Vec::as_slice),
        Some(&[Value::String(tab)][..]),
        "the sharing device's plane names the window it is asked for:{}",
        two.said()
    );

    // The app hands its page over: the watching device says the window is pictured once its tile
    // holds the page, and the sharing device that it sends it.
    assert!(
        two.within(LINK, true, |two| {
            two.received()
                .is_some_and(|stream| stream["pictured"].as_bool() == Some(true))
        }),
        "the watching device never said the window is pictured:{}",
        two.said()
    );
    assert_eq!(
        two.tile().map(|tile| tile.standing),
        Some(Standing::Pictured),
        "pictured is said of a tile that holds the page"
    );
    let received = two
        .received()
        .expect("the watching device holds the window");
    assert_eq!(
        received["waiting_for"].as_str(),
        Some(""),
        "a pictured window waits for nothing:{}",
        two.said()
    );
    assert!(
        two.within(LINK, true, |two| {
            two.shared()
                .is_some_and(|shared| shared["streaming"].as_bool() == Some(true))
        }),
        "the sharing device never said it sends the window:{}",
        two.said()
    );
    let shared = two.shared().expect("the sharing device shares the window");
    assert_eq!(
        shared["waiting_for"].as_str(),
        Some(""),
        "a window that is sent waits for nothing:{}",
        two.said()
    );
}

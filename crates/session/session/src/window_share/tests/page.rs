//! A window whose app draws its own page, shared with the workspace. Nothing of such a window is
//! captured: its app hands the session the page as a display list, the workspace's live plane
//! carries it on a track of its own, and the device showing the window draws it in the tab its
//! desk lists the window in. The test plays the app: it says its window runs a page engine, and
//! hands over what a page engine captured of a page. Where a test is about what the session and
//! the app say to each other, the app speaks on a connection of its own, made as the launcher
//! makes an app's first window's ([`App`]).

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;

use tilo_desktop_surface::{PeerRead, Standing};
use tilo_live::live::Track;
use tilo_live::page::PageObject;
use ui::floating_surface::{FloatingSurfaceClientMessage, FloatingSurfaceHostMessage, PaintPart};
use ui::webview::paint::PaintEngine;
use ui::webview::paint::stream::Item;
use ui::webview::paint::stream::wire::{CODEC, Payload, Receiver};
use ui::webview::test_engine::TestEngine;
use ui::webview::{WebViewCommand, WebViewEngine};

use super::*;

/// The engine's page: a snapshot with a header, a sidebar and a button.
const TOP: &str =
    include_str!("../../../../../browser/paint-stream/tests/fixtures/layers.top.json");

/// How long one link of the page's way may take on a machine busy with the rest of the suite.
const LINK: Duration = Duration::from_secs(120);

/// How long the app may wait to be asked for its page once a desk asks the plane for it. The ask
/// is one pass of the session; this is what a machine busy with the rest of the suite may take.
const ASKED: Duration = Duration::from_secs(30);

/// How long the two devices may take to say that a page is overdue, past the bounds themselves
/// ([`crate::page_track::HAND_OVER_BOUND`] and the watching side's, five seconds longer), on a
/// machine busy with the rest of the suite.
const SAID: Duration = Duration::from_secs(120);

/// How often the app hands its page over again. An app answers each ask for a whole frame; this
/// one cannot hear the ask, so it hands the whole page over while it is wanted.
const HANDED_EVERY: Duration = Duration::from_millis(250);

/// What the window's app hands the session of its page, each payload with whether it is a whole
/// frame: the whole frame, then the images it names.
fn page_objects() -> Vec<(bool, Vec<u8>)> {
    page_objects_of(TOP)
}

/// [`page_objects`] of the page an engine captured as `snapshot`.
fn page_objects_of(snapshot: &str) -> Vec<(bool, Vec<u8>)> {
    let test = TestEngine::new();
    test.set_paint_snapshot(snapshot);
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

/// The app of the shared window on its own connection to the session, made before the app started
/// as the launcher makes every app's first window's: the connection names no tab, and the session
/// finds the window it speaks for on the desk.
struct App {
    stream: UnixStream,
    /// What arrived and is not a whole frame yet.
    unread: Vec<u8>,
    /// What the session told the window, oldest first.
    told: Vec<FloatingSurfaceHostMessage>,
}

impl App {
    fn launched(session: &Session) -> Self {
        let stream = session
            .tabs
            .as_ref()
            .expect("the session serves its apps' windows")
            .launched_window(APP, WORKSPACE)
            .expect("the window's connection");
        Self::on(stream)
    }

    /// The app's connection for one more window, which the session opens in the app's running
    /// process and connects itself: the connection names the tab the window takes.
    fn opened(session: &Session, tab: u64) -> Self {
        let stream = session
            .tabs
            .as_ref()
            .expect("the session serves its apps' windows")
            .opened_window(APP, WORKSPACE, tab)
            .expect("the window's connection");
        Self::on(stream)
    }

    fn on(stream: UnixStream) -> Self {
        // A read that finds nothing returns to the pass that asked.
        stream
            .set_read_timeout(Some(Duration::from_millis(1)))
            .expect("a read that does not wait");
        Self {
            stream,
            unread: Vec::new(),
            told: Vec::new(),
        }
    }

    /// Write one message to the session, as the app's toolkit frames it.
    fn say(&mut self, message: &FloatingSurfaceClientMessage) {
        let mut frame = serde_json::to_vec(message).expect("a message");
        frame.push(b'\n');
        self.stream.write_all(&frame).expect("the session reads");
    }

    /// Hand over `objects` of the window's page, each in one message, as the window `instance`
    /// where the toolkit of the window names it.
    fn hand_over(&mut self, objects: &[(bool, Vec<u8>)], instance: Option<u64>) {
        use base64::Engine as _;
        for (_, bytes) in objects {
            self.say(&FloatingSurfaceClientMessage::PaintStream {
                payload: base64::engine::general_purpose::STANDARD.encode(bytes),
                page: [0.0, 0.0, 800.0, 600.0],
                scale: 1.0,
                instance,
                part: None,
            });
        }
    }

    /// Hand over the first `parts` parts of one payload of `of` bytes that starts with `bytes`,
    /// as the toolkit cuts a payload too large for one message
    /// (`ui::webview::paint::MAX_SHARE_PART`).
    fn hand_over_parts(&mut self, bytes: &[u8], of: usize, parts: usize) {
        use base64::Engine as _;
        let mut at = 0_usize;
        for part in bytes.chunks(ui::webview::paint::MAX_SHARE_PART).take(parts) {
            self.say(&FloatingSurfaceClientMessage::PaintStream {
                payload: String::new(),
                page: [0.0, 0.0, 800.0, 600.0],
                scale: 1.0,
                instance: None,
                part: Some(PaintPart {
                    bytes: base64::engine::general_purpose::STANDARD.encode(part),
                    at: at as u64,
                    of: of as u64,
                }),
            });
            at += part.len();
        }
    }

    /// Whether the session asked the window, here, for a whole frame of its page.
    fn asked(&self) -> bool {
        self.told
            .contains(&FloatingSurfaceHostMessage::PaintKeyframe)
    }

    /// Read what the session wrote since the last pass.
    fn hear(&mut self) {
        let mut chunk = [0_u8; 16 * 1024];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => self.unread.extend_from_slice(&chunk[..read]),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(error) => panic!("the window's connection failed: {error}"),
            }
        }
        while let Some(end) = self.unread.iter().position(|byte| *byte == b'\n') {
            let frame: Vec<u8> = self.unread.drain(..=end).collect();
            let message = serde_json::from_slice(&frame[..end])
                .expect("the session writes messages the app reads");
            self.told.push(message);
        }
    }

    /// What the session told the window, by the names the messages go by; an ask it did not
    /// answer with which one and why.
    fn told(&self) -> Vec<String> {
        self.told
            .iter()
            .map(|message| {
                let value = serde_json::to_value(message).unwrap_or_default();
                let name = value["type"].as_str().unwrap_or_default().to_owned();
                match value["reason"].as_str() {
                    Some(reason) => format!(
                        "{name} ({}: {reason})",
                        value["message"].as_str().unwrap_or_default()
                    ),
                    None => name,
                }
            })
            .collect()
    }
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
    /// The window's app on its own connection, where the test gave it one.
    app: Option<App>,
}

impl Two {
    /// The device of the first seed opens a window whose app says it draws a page, and shares it
    /// with the workspace; the device of the second lists the window as one that runs on the
    /// first, and reads the share from the workspace's governance.
    fn sharing(seeds: [u8; 2]) -> Self {
        Self::made(seeds, false)
    }

    /// As [`Self::sharing`], with the window's app on a connection of its own: there it says that
    /// it draws a page, and there it is asked for it.
    fn sharing_from_its_app(seeds: [u8; 2]) -> Self {
        Self::made(seeds, true)
    }

    fn made(seeds: [u8; 2], connected: bool) -> Self {
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
        // The window's app runs a page engine, and said so when its window opened.
        let (tab, app) = if connected {
            let mut app = App::launched(&host.session);
            app.say(&FloatingSurfaceClientMessage::PaintCapable);
            // The connection names no tab: the session finds the window the app speaks for, its
            // window in front, and offers the page of the tab that window stands in.
            let client = window.as_mut().expect("the window's client");
            let deadline = Instant::now() + Duration::from_secs(60);
            let tab = loop {
                client.exchange();
                host.session.step(Duration::from_millis(2)).expect("step");
                app.hear();
                let offered = host.session.shared.pages.borrow().offered();
                if let [tab] = offered[..] {
                    break tab;
                }
                assert!(
                    Instant::now() < deadline,
                    "the app said its window draws a page, and the session offers the pages of \
                     {offered:?}; it told the app {:?}",
                    app.told()
                );
            };
            assert!(
                host.session.shared.pages.borrow().streamed(tab, WORKSPACE),
                "the page is offered in the workspace the app runs in"
            );
            let tree = host.session.handle().read(Clone::clone);
            assert!(
                crate::window_share::window_shown(host.session.compositor(), &tree, tab).is_some(),
                "the tab the session found for the app is the one its window stands in"
            );
            (tab, Some(app))
        } else {
            let tab = tab_of(&host.session, APP);
            host.session
                .shared
                .pages
                .borrow_mut()
                .capable(tab, WORKSPACE);
            (tab, None)
        };
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
            app,
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
        if let Some(app) = self.app.as_mut() {
            app.hear();
        }
    }

    /// Whether the session asked the window's app, on its connection, for a whole frame of its
    /// page.
    fn app_asked(&self) -> bool {
        self.app.as_ref().is_some_and(|app| {
            app.told
                .contains(&FloatingSurfaceHostMessage::PaintKeyframe)
        })
    }

    /// The app answers the ask on its connection: the whole frame of its page, then the images it
    /// names, as its toolkit sends them.
    fn app_hands_over(&mut self) {
        use base64::Engine as _;
        let app = self.app.as_mut().expect("the window's app is connected");
        for (_, bytes) in &self.page {
            app.say(&FloatingSurfaceClientMessage::PaintStream {
                payload: base64::engine::general_purpose::STANDARD.encode(bytes),
                page: [0.0, 0.0, 800.0, 600.0],
                scale: 1.0,
                instance: None,
                part: None,
            });
        }
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

    // What the person at the watching desk is shown, as anything that reads a desk by the names
    // on it reads it: the text of the page, in the tab that draws it.
    let named = |two: &Two| -> Vec<String> {
        two.viewer
            .session
            .controls()
            .into_iter()
            .map(|control| control.name)
            .collect()
    };
    assert!(
        two.within(LINK, true, |two| {
            let named = named(two);
            ["Paint stream", "Alpha"]
                .iter()
                .all(|shown| named.iter().any(|name| name.contains(shown)))
        }),
        "the watching desk does not name the text of the page it draws: {:?}",
        named(&two)
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

/// The app of a shared window is asked for its page where it listens: on the connection its
/// launcher made for its window, which names no tab. When a desk asks the plane for the page, the
/// session writes the ask for a whole frame to that connection; what the app answers there is
/// published, and the watching desk draws it. Nothing here hands the session a page behind the
/// app's back.
#[test]
fn the_app_of_a_shared_page_is_asked_for_it_on_its_own_connection() {
    let mut two = Two::sharing_from_its_app([0xc5, 0xc6]);

    // Announced and asked for: the watching desk asks the plane for the page's track, and the
    // sharing session reads that the page is wanted.
    assert!(
        two.within(LINK, false, |two| {
            two.received().is_some() && two.tile().is_some() && two.wanted() == [two.tab]
        }),
        "the page was never announced and asked for:{}",
        two.said()
    );

    // The app is asked for a whole frame of it.
    let asked = two.within(ASKED, false, Two::app_asked);
    assert!(
        asked,
        "a desk asks the plane for the window's page, and the session never asked the window's \
         app for it on the app's connection, which heard {:?}:{}",
        two.app.as_ref().map(App::told),
        two.said()
    );

    // It answers there, and the watching desk draws what it handed over.
    two.app_hands_over();
    assert!(
        two.within(LINK, false, |two| {
            two.tile()
                .is_some_and(|tile| tile.standing == Standing::Pictured)
        }),
        "the app handed its page over on its connection, and the watching desk's tile still \
         shows the arriving card:{}",
        two.said()
    );
    assert!(
        two.within(LINK, false, |two| {
            two.shared()
                .is_some_and(|shared| shared["streaming"].as_bool() == Some(true))
        }),
        "the sharing device never said it sends the window:{}",
        two.said()
    );
}

/// An app that is asked for its page and hands nothing over is said to, on both devices: the
/// sharing one says that the window's app was asked and has not handed its page over, in other
/// words than a page that is merely on its way, and asks the app again; the watching one says
/// that the device running the window was asked and sent none. Once the app hands its page over,
/// both wait for nothing.
#[test]
fn an_app_that_hands_nothing_over_is_said_to_withhold_its_page() {
    use tilo_desktop_surface::i18n::t;

    let (withheld, overdue) = (
        t("session-window-share-page-withheld"),
        t("session-window-share-page-overdue"),
    );
    for said in [&withheld, &overdue] {
        assert!(
            !said.trim().is_empty() && !said.starts_with("session-window-share"),
            "the catalogue has the words: {said}"
        );
    }
    assert_ne!(withheld, t("session-window-share-page-unsent"));
    assert_ne!(overdue, t("session-window-share-first-page"));

    let mut two = Two::sharing_from_its_app([0xc7, 0xc8]);
    assert!(
        two.within(LINK, false, |two| {
            two.received().is_some() && two.tile().is_some() && two.app_asked()
        }),
        "the window's app was never asked for its page:{}",
        two.said()
    );

    // The app answers nothing. The sharing device says so once the app's time is up, and asks
    // it again.
    assert!(
        two.within(SAID, false, |two| {
            two.shared()
                .is_some_and(|shared| shared["waiting_for"].as_str() == Some(withheld.as_str()))
        }),
        "the sharing device never said its window's app has not handed the page over:{}",
        two.said()
    );
    let shared = two.shared().expect("the sharing device shares the window");
    assert_eq!(
        shared["streaming"].as_bool(),
        Some(false),
        "a window whose app handed nothing over goes out to nobody:{}",
        two.said()
    );
    let asks = two.app.as_ref().map_or(0, |app| {
        app.told
            .iter()
            .filter(|told| **told == FloatingSurfaceHostMessage::PaintKeyframe)
            .count()
    });
    assert!(
        asks >= 2,
        "an app that did not answer is asked again: it was asked {asks} times"
    );

    // The watching device says which side its window waits on.
    assert!(
        two.within(SAID, false, |two| {
            two.received()
                .is_some_and(|stream| stream["waiting_for"].as_str() == Some(overdue.as_str()))
        }),
        "the watching device never said the device running the window sent no page:{}",
        two.said()
    );
    assert_eq!(
        two.received().expect("the start")["pictured"].as_bool(),
        Some(false)
    );

    // The app hands its page over at last: both devices wait for nothing.
    two.app_hands_over();
    assert!(
        two.within(LINK, false, |two| {
            two.received().is_some_and(|stream| {
                stream["pictured"].as_bool() == Some(true)
                    && stream["waiting_for"].as_str() == Some("")
            })
        }),
        "the watching device still waits for a page its tile holds:{}",
        two.said()
    );
    assert!(
        two.within(LINK, false, |two| {
            two.shared().is_some_and(|shared| {
                shared["streaming"].as_bool() == Some(true)
                    && shared["waiting_for"].as_str() == Some("")
            })
        }),
        "the sharing device still says its app withholds a page it handed over:{}",
        two.said()
    );
}

/// A whole frame of the page in [`TOP`] made as large as a long article's: the same page with its
/// text set down again and again, encoded as the engine encodes it. More than one message of the
/// app's connection carries, and less than the plane's one object.
fn large_whole_frame() -> Vec<u8> {
    let (_, key) = page_objects()
        .into_iter()
        .find(|(whole, _)| *whole)
        .expect("the page's whole frame");
    let Payload::Key(mut frame) = Payload::decode(&key).expect("a whole frame") else {
        panic!("the whole frame is a key");
    };
    let layer = frame
        .layers
        .values_mut()
        .find(|layer| {
            layer
                .items
                .iter()
                .any(|item| matches!(item, Item::Text { .. }))
        })
        .expect("a layer with text");
    let text = layer
        .items
        .iter()
        .find(|item| matches!(item, Item::Text { .. }))
        .cloned()
        .expect("a run of text");
    let one = ui::webview::paint::stream::encode(&text)
        .expect("an item")
        .len()
        .max(1);
    let wanted = ui::webview::paint::MAX_SHARE_PART * 2 + 100_000;
    for _ in 0..wanted / one {
        layer.items.push(text.clone());
    }
    let bytes = ui::webview::paint::stream::encode(&Payload::Key(frame)).expect("a payload");
    assert!(
        bytes.len() > 2 * ui::webview::paint::MAX_SHARE_PART
            && bytes.len() <= crate::page_track::MAX_PAYLOAD,
        "the page is one of three parts that the plane carries as one object: {} bytes",
        bytes.len()
    );
    bytes
}

/// A page whose whole frame is larger than one message of the app's connection carries arrives:
/// the app hands it over in parts on its connection, the session joins them and publishes the
/// page as one object, and the watching desk draws it. Parts that do not follow each other are
/// published as nothing, and the app is asked for the page again.
#[test]
fn a_page_handed_over_in_parts_is_drawn_whole_on_the_watching_desk() {
    let mut two = Two::sharing_from_its_app([0xd1, 0xd2]);
    let large = large_whole_frame();
    assert!(
        two.within(LINK, false, |two| {
            two.received().is_some() && two.tile().is_some() && two.app_asked()
        }),
        "the window's app was never asked for its page:{}",
        two.said()
    );

    // A payload that breaks off: its first part, then its third. Nothing of it is published.
    let asks = |two: &Two| {
        two.app.as_ref().map_or(0, |app| {
            app.told
                .iter()
                .filter(|told| **told == FloatingSurfaceHostMessage::PaintKeyframe)
                .count()
        })
    };
    let before = two.opened();
    {
        let app = two.app.as_mut().expect("the window's app is connected");
        app.hand_over_parts(&large, large.len(), 1);
        use base64::Engine as _;
        let third = &large[2 * ui::webview::paint::MAX_SHARE_PART..];
        app.say(&FloatingSurfaceClientMessage::PaintStream {
            payload: String::new(),
            page: [0.0, 0.0, 800.0, 600.0],
            scale: 1.0,
            instance: None,
            part: Some(PaintPart {
                bytes: base64::engine::general_purpose::STANDARD.encode(third),
                at: (2 * ui::webview::paint::MAX_SHARE_PART) as u64,
                of: large.len() as u64,
            }),
        });
    }
    for _ in 0..16 {
        two.pass(false);
    }
    assert_eq!(
        two.opened(),
        before,
        "parts that do not follow each other are published as nothing:{}",
        two.said()
    );
    assert_eq!(
        two.tile().map(|tile| tile.standing),
        Some(Standing::Started),
        "and the watching desk draws nothing of them"
    );

    // The whole payload, in its parts, and the images its page names.
    let images: Vec<(bool, Vec<u8>)> = two
        .page
        .iter()
        .filter(|(whole, _)| !*whole)
        .cloned()
        .collect();
    {
        let app = two.app.as_mut().expect("the window's app is connected");
        app.hand_over_parts(&large, large.len(), usize::MAX);
        app.hand_over(&images, None);
    }
    assert!(
        two.within(LINK, false, |two| {
            two.tile()
                .is_some_and(|tile| tile.standing == Standing::Pictured)
        }),
        "a page of {} bytes was handed over in parts, and the watching desk's tile still shows \
         the arriving card; the app was asked {} times:{}",
        large.len(),
        asks(&two),
        two.said()
    );
    assert!(
        two.within(LINK, false, |two| {
            two.shared()
                .is_some_and(|shared| shared["streaming"].as_bool() == Some(true))
        }),
        "the sharing device never said it sends the window:{}",
        two.said()
    );
    let named: Vec<String> = two
        .viewer
        .session
        .controls()
        .into_iter()
        .map(|control| control.name)
        .collect();
    assert!(
        named.iter().any(|name| name.contains("Paint stream")),
        "the watching desk names the text of the page it draws: {named:?}"
    );
}

/// A page too large for the workspace to carry is said to be, on both devices, and nobody waits
/// for it in silence. The app says how large its whole frame is with the first part of it; the
/// sharing device says the page is too large to send, in other words than a page on its way or
/// one its app withholds, and sends the window to nobody; the watching device says the device
/// running the window sent no page. When the page becomes one the workspace carries, it arrives
/// and both wait for nothing.
#[test]
fn a_page_too_large_to_carry_is_said_to_be_on_both_devices() {
    use tilo_desktop_surface::i18n::t;

    let (too_large, overdue) = (
        t("session-window-share-page-too-large"),
        t("session-window-share-page-overdue"),
    );
    assert!(
        !too_large.trim().is_empty() && !too_large.starts_with("session-window-share"),
        "the catalogue has the words: {too_large}"
    );
    for other in [
        t("session-window-share-page-unsent"),
        t("session-window-share-page-withheld"),
    ] {
        assert_ne!(too_large, other);
    }

    let mut two = Two::sharing_from_its_app([0xd3, 0xd4]);
    assert!(
        two.within(LINK, false, |two| {
            two.received().is_some() && two.tile().is_some() && two.app_asked()
        }),
        "the window's app was never asked for its page:{}",
        two.said()
    );

    // The app's whole frame is one byte more than the plane carries: it says so with the first
    // part, and sends no more of it.
    let start = large_whole_frame();
    let before = two.opened();
    two.app
        .as_mut()
        .expect("the window's app is connected")
        .hand_over_parts(&start, crate::page_track::MAX_PAYLOAD + 1, 1);
    assert!(
        two.within(ASKED, false, |two| {
            two.shared()
                .is_some_and(|shared| shared["waiting_for"].as_str() == Some(too_large.as_str()))
        }),
        "the sharing device never said its window's page is too large to send:{}",
        two.said()
    );
    let shared = two.shared().expect("the sharing device shares the window");
    assert_eq!(
        shared["streaming"].as_bool(),
        Some(false),
        "a page too large goes out to nobody:{}",
        two.said()
    );
    assert_eq!(two.opened(), before, "nothing of it was published");

    // The watching device says which side its window waits on.
    assert!(
        two.within(SAID, false, |two| {
            two.received()
                .is_some_and(|stream| stream["waiting_for"].as_str() == Some(overdue.as_str()))
        }),
        "the watching device never said the device running the window sent no page:{}",
        two.said()
    );
    assert_eq!(
        two.received().expect("the start")["pictured"].as_bool(),
        Some(false)
    );
    assert_eq!(
        two.shared().expect("the share")["waiting_for"].as_str(),
        Some(too_large.as_str()),
        "the app is asked again and its page is still too large:{}",
        two.said()
    );

    // The page became one the workspace carries.
    two.app_hands_over();
    assert!(
        two.within(LINK, false, |two| {
            two.received().is_some_and(|stream| {
                stream["pictured"].as_bool() == Some(true)
                    && stream["waiting_for"].as_str() == Some("")
            }) && two.shared().is_some_and(|shared| {
                shared["streaming"].as_bool() == Some(true)
                    && shared["waiting_for"].as_str() == Some("")
            })
        }),
        "the page the workspace carries did not end what both devices said:{}",
        two.said()
    );
}

/// The text of the page `receiver` holds, run by run.
fn texts(receiver: &Receiver) -> Vec<String> {
    receiver
        .frame()
        .map(|frame| {
            frame
                .layers
                .values()
                .flat_map(|layer| &layer.items)
                .filter_map(|item| match item {
                    Item::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A device of the workspace that only watches: its plane, asked for the pages of some of the
/// sharing device's windows, and the page it holds of each, joined from what the plane brought
/// on that window's track. What a desk watching a window is shown is what its track carries.
struct Watcher {
    plane: Arc<Plane>,
    host: [u8; 32],
    held: BTreeMap<u64, Receiver>,
}

impl Watcher {
    fn of(seed: u8, host: EndpointId) -> Self {
        let plane = live_plane(device(seed))
            .remove(WORKSPACE)
            .expect("the watching device's plane");
        Self {
            plane,
            host: *host.as_bytes(),
            held: BTreeMap::new(),
        }
    }

    /// Ask the sharing device for the pages of `tabs`, and of nothing else.
    fn show(&self, tabs: &[u64]) {
        let tracks: Vec<Track> = tabs.iter().map(|tab| Track::Page(*tab)).collect();
        self.plane.show(self.host, &tracks);
    }

    /// Take what the plane brought.
    fn hear(&mut self) {
        while let Some(heard) = self.plane.try_recv() {
            let Track::Page(tab) = heard.track else {
                continue;
            };
            if heard.sender != self.host {
                continue;
            }
            let object = PageObject::decode(&heard.body).expect("a page object");
            let payload = Payload::decode(&object.payload).expect("a payload");
            drop(self.held.entry(tab).or_default().apply(payload));
        }
    }

    /// The text of the page held of the window `tab`.
    fn texts(&self, tab: u64) -> Vec<String> {
        self.held.get(&tab).map(texts).unwrap_or_default()
    }

    /// Whether the page held of `tab` says `shown` and nothing of `other`.
    fn holds(&self, tab: u64, shown: &str, other: &str) -> bool {
        let texts = self.texts(tab);
        texts.iter().any(|text| text.contains(shown))
            && !texts.iter().any(|text| text.contains(other))
    }
}

/// One device with two windows of one app, each with a page of its own, and a device that
/// watches.
struct TwoWindows {
    host: Box<Member>,
    clients: Vec<TestClient>,
    viewer: Box<Member>,
    watcher: Watcher,
    /// The tabs the two windows stand in: the one in front, and the other.
    front: u64,
    back: u64,
}

impl TwoWindows {
    fn opened(seeds: [u8; 3]) -> Self {
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
        let mut clients = vec![mapped(&mut host.session, APP)];
        let mut second = TestClient::connect(host.session.compositor());
        until_window(&mut host.session, &mut second, |_, seen| {
            seen.wm_base.is_some() && seen.seat.is_some()
        });
        second.open_window(APP);
        until_window(&mut host.session, &mut second, |_, seen| seen.configured);
        let (w, h) = second
            .seen
            .size
            .filter(|(w, h)| *w > 0 && *h > 0)
            .unwrap_or((300, 200));
        second.show(w, h, [40, 40, 200, 255]);
        clients.push(second);
        // The desk gives each window of the app a tab of its own.
        let deadline = Instant::now() + Duration::from_secs(60);
        let tabs = loop {
            for client in &mut clients {
                client.exchange();
            }
            host.session.step(Duration::from_millis(2)).expect("step");
            let tabs = windows_of(&mut host.session, APP);
            if tabs.len() == 2 {
                break tabs;
            }
            assert!(
                Instant::now() < deadline,
                "two windows of the app are open, and the desk shows them in the tabs {tabs:?}"
            );
        };
        let tree = host.session.handle().read(Clone::clone);
        let front = tilo_desktop_surface::navigation::active_route(&tree)
            .map(|route| route.tab)
            .filter(|tab| tabs.contains(tab))
            .unwrap_or(tabs[0]);
        let back = *tabs
            .iter()
            .find(|tab| **tab != front)
            .expect("the other window");
        let viewer = Box::new(member_minting_under(
            seeds[1],
            showing(device(seeds[0]), APP, front),
            1,
        ));
        let watcher = Watcher::of(seeds[2], device(seeds[0]));
        Self {
            host,
            clients,
            viewer,
            watcher,
            front,
            back,
        }
    }

    /// One pass of the windows' clients and of both sessions, with what their planes said carried
    /// after it; the apps in `apps` hear what the session told them.
    fn pass(&mut self, apps: &mut [&mut App]) {
        for client in &mut self.clients {
            client.exchange();
        }
        self.host
            .session
            .step(Duration::from_millis(2))
            .expect("step");
        for client in &mut self.clients {
            client.exchange();
        }
        self.viewer
            .session
            .step(Duration::from_millis(2))
            .expect("step");
        carry(&mut self.host, &self.viewer);
        carry(&mut self.viewer, &self.host);
        for app in apps.iter_mut() {
            app.hear();
        }
        self.watcher.hear();
    }

    /// Pass until `done` holds, for at most `limit`: whether it came to hold.
    fn within(
        &mut self,
        limit: Duration,
        apps: &mut [&mut App],
        mut done: impl FnMut(&Self, &[&mut App]) -> bool,
    ) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            self.pass(apps);
            if done(self, apps) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
        }
    }

    /// The windows whose page the sharing session offers.
    fn offered(&self) -> Vec<u64> {
        self.host.session.shared.pages.borrow().offered()
    }

    /// Share the window in `tab` with the workspace.
    fn share(&mut self, tab: u64) {
        let tree = self.host.session.handle().read(Clone::clone);
        let (window, app, _) =
            crate::window_share::window_shown(self.host.session.compositor(), &tree, tab)
                .expect("the tab shows a window");
        self.host
            .session
            .invoke(
                SHARES_RESOURCE,
                "share",
                &arguments(serde_json::json!({
                    "window": window,
                    "tab": tab.to_string(),
                    "app_id": app,
                    "room": WORKSPACE,
                })),
            )
            .expect("the window is shared with the workspace");
    }
}

/// The tabs of the desk in which a window of `app` stands, in order.
fn windows_of(session: &mut Session, app: &str) -> Vec<u64> {
    let tree = session.handle().read(Clone::clone);
    let listed: Vec<u64> = tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| tab.id)
        .collect();
    let mut tabs = Vec::new();
    for tab in listed {
        if crate::window_share::window_shown(session.compositor(), &tree, tab)
            .is_some_and(|(_, shown, _)| shown == app)
        {
            tabs.push(tab);
        }
    }
    tabs.sort_unstable();
    tabs
}

/// Each window of an app with several is asked for its own page and published as itself. The
/// first window speaks on the connection its launcher made, which names no tab, and is not the
/// window in front: its offer is taken up for no window until it says which one it is, and then
/// for that one, never for the window in front. The second speaks on a connection the session
/// made for it. A device that watches both is shown each window's own page on that window's
/// track, and a frame that names another window than its connection's is published as nobody's.
#[test]
fn each_window_of_an_app_is_asked_for_its_own_page_and_published_as_itself() {
    let mut two = TwoWindows::opened([0xd5, 0xd6, 0xd7]);
    let (front, back) = (two.front, two.back);
    let one = page_objects();
    let other = page_objects_of(&TOP.replace("Alpha", "Omega"));

    // The first window's app says it draws a page, on a connection that names no tab, while two
    // windows of the app stand on the desk: which of them speaks is not known, so the page is
    // offered as neither's. Before, it was offered as the page of the window in front.
    let mut first = App::launched(&two.host.session);
    first.say(&FloatingSurfaceClientMessage::PaintCapable);
    for _ in 0..16 {
        two.pass(&mut [&mut first]);
    }
    assert!(
        two.offered().is_empty(),
        "a connection that names no window offered a page, and the session took it for the page \
         of {:?}; the window in front is {front}",
        two.offered()
    );
    assert!(
        first
            .told()
            .iter()
            .all(|told| !told.starts_with("unanswered")),
        "the offer stands until the window names itself: {:?}",
        first.told()
    );

    // The window says which one it is, as every window does in what it declares for its bar:
    // the one that is not in front.
    first.say(&FloatingSurfaceClientMessage::Toolbar {
        toolbar: None,
        instance: Some(back),
    });
    assert!(
        two.within(ASKED, &mut [&mut first], |two, _| two.offered() == [back]),
        "the window named itself as {back}, and the session offers the pages of {:?}",
        two.offered()
    );

    // The second window, on a connection the session made for it.
    let mut second = App::opened(&two.host.session, front);
    second.say(&FloatingSurfaceClientMessage::PaintCapable);
    let mut both = [front, back];
    both.sort_unstable();
    assert!(
        two.within(ASKED, &mut [&mut first, &mut second], |two, _| {
            two.offered() == both
        }),
        "both windows offered their page, and the session offers the pages of {:?}",
        two.offered()
    );

    // Both are shared, and a device watches both.
    two.share(front);
    two.share(back);
    assert!(
        two.within(LINK, &mut [&mut first, &mut second], |two, _| {
            rows(&shares(&two.host.session), "sharing").len() == 2
        }),
        "both windows are shared: {}",
        shares(&two.host.session)
    );
    carry_governance(&two.host.session, &two.viewer.session);
    two.watcher.show(&[front, back]);
    assert!(
        two.within(LINK, &mut [&mut first, &mut second], |_, apps| {
            apps.iter().all(|app| app.asked())
        }),
        "each window is asked for its page on its own connection: the first heard {:?}, the \
         second {:?}; the session says {}",
        first.told(),
        second.told(),
        shares(&two.host.session)
    );

    // Each hands over its own page, the first naming itself as its toolkit does.
    first.hand_over(&one, Some(back));
    second.hand_over(&other, None);
    assert!(
        two.within(LINK, &mut [&mut first, &mut second], |two, _| {
            two.watcher.holds(back, "Alpha", "Omega") && two.watcher.holds(front, "Omega", "Alpha")
        }),
        "a device watching both windows holds {:?} of the window in front and {:?} of the other",
        two.watcher.texts(front),
        two.watcher.texts(back)
    );

    // A frame that names the other window, on the first window's connection: published as
    // nobody's page, and the app is told.
    first.hand_over(&one, Some(front));
    assert!(
        two.within(ASKED, &mut [&mut first, &mut second], |_, apps| {
            apps[0]
                .told()
                .iter()
                .any(|told| told.starts_with("unanswered (paint_stream"))
        }),
        "a frame naming another window was not refused: {:?}",
        first.told()
    );
    for _ in 0..16 {
        two.pass(&mut [&mut first, &mut second]);
    }
    assert!(
        two.watcher.holds(front, "Omega", "Alpha") && two.watcher.holds(back, "Alpha", "Omega"),
        "the window in front is still shown its own page: {:?}, and the other its own: {:?}",
        two.watcher.texts(front),
        two.watcher.texts(back)
    );

    // An older toolkit names no window in a frame: it is the page of its connection's window,
    // whichever window is in front.
    second.hand_over(&other, None);
    first.hand_over(&one, None);
    for _ in 0..16 {
        two.pass(&mut [&mut first, &mut second]);
    }
    assert!(
        two.watcher.holds(front, "Omega", "Alpha") && two.watcher.holds(back, "Alpha", "Omega"),
        "frames that name no window stay their connection's: {:?} and {:?}",
        two.watcher.texts(front),
        two.watcher.texts(back)
    );
}

/// The page of a window nobody shared reaches nobody. The workspace's plane takes any member's
/// ask for any window's page track, so a device asks for the page of a Browser window that was
/// never shared: the session publishes nothing, and the window's app is never asked for it.
#[test]
fn the_page_of_a_window_nobody_shared_reaches_nobody_who_asks_for_it() {
    let salt = tilo_desktop_model::ids::device_salt_of_key(device(0xd8).as_bytes());
    let mut host = Box::new(member_minting_under(
        0xd8,
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
    let mut client = mapped(&mut host.session, APP);
    let mut app = App::launched(&host.session);
    app.say(&FloatingSurfaceClientMessage::PaintCapable);
    let mut pass = |host: &mut Member, app: &mut App| {
        client.exchange();
        host.session.step(Duration::from_millis(2)).expect("step");
        app.hear();
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let tab = loop {
        pass(&mut host, &mut app);
        if let [tab] = host.session.shared.pages.borrow().offered()[..] {
            break tab;
        }
        assert!(
            Instant::now() < deadline,
            "the app's page was never offered"
        );
    };

    // Another device of the workspace asks the plane for that window's page.
    let mut watcher = Watcher::of(0xd9, device(0xd8));
    watcher.show(&[tab]);
    let plane = crate::calls::planes_of(&host.sync)
        .remove(WORKSPACE)
        .expect("the sharing device's plane");
    let deadline = Instant::now() + Duration::from_secs(60);
    while plane.wanted_pages() != [tab] {
        pass(&mut host, &mut app);
        assert!(
            Instant::now() < deadline,
            "the ask for the window's page never reached its device's plane"
        );
    }
    for _ in 0..32 {
        pass(&mut host, &mut app);
        watcher.hear();
    }
    assert!(
        !app.asked(),
        "the app of a window nobody shared was asked for its page: {:?}",
        app.told()
    );
    assert!(
        host.session.shared.pages.borrow().viewed().is_empty(),
        "a window nobody shared is shown to nobody"
    );
    // What the app hands over unasked goes nowhere either.
    app.hand_over(&page_objects(), None);
    for _ in 0..32 {
        pass(&mut host, &mut app);
        watcher.hear();
    }
    assert!(
        watcher.texts(tab).is_empty(),
        "the page of a window nobody shared reached a device that asked for it: {:?}",
        watcher.texts(tab)
    );
}

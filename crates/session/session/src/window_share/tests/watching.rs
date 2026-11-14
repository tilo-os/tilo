//! What the sharing device tells its person of who sees a shared window. A device that asked to
//! see a window is not yet one that sees it: until something of the window goes out, what the bar
//! is told, what its people control tells a screen reader and the window's menu all say that the
//! device waits, and none says that anyone watches. They follow what the share's own reading says
//! goes out (`streaming`), for a window shared as its page and for one shared as pictures.

use ui::webview::paint::PaintEngine;
use ui::webview::paint::stream::wire::Payload;
use ui::webview::test_engine::TestEngine;
use ui::webview::{WebViewCommand, WebViewEngine};

use tilo_desktop_surface::WorkspaceShare;

use super::menu::{STOP_ROW, label_of, menu_of};
use super::*;

/// The engine's page: a snapshot with a header, a sidebar and a button.
const TOP: &str =
    include_str!("../../../../../browser/paint-stream/tests/fixtures/layers.top.json");

/// How long the bar may take to say what the shares say, and a page to go out once its app handed
/// it over, on a machine busy with the rest of the suite.
const SAID: Duration = Duration::from_secs(120);

/// How often the app hands its page over again while it is wanted.
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

/// The desk of the device `seed` : one window of the app, in a tab that device minted, which is
/// the window its menu offers to share. With the salt its ids are minted under.
fn desk_of(seed: u8) -> (DesktopTree, u16) {
    let salt = tilo_desktop_model::ids::device_salt_of_key(device(seed).as_bytes());
    let desk = desk(
        TileContent::LocalApp {
            app_id: APP.to_owned(),
            label: "Notes".to_owned(),
            note: String::new(),
        },
        tilo_desktop_model::ids::compose_id(salt, 1),
    );
    (desk, salt)
}

/// One device of the workspace whose session is in no live plane of it: it holds the workspace
/// and hears its members, and nothing of what it shares has a way out.
fn member_outside_the_plane(seed: u8, desk: DesktopTree, salt: u16) -> Member {
    let (sync, said) = Client::planeless(
        device(seed),
        OwnPresence {
            label: format!("Device {seed:x}"),
            ..OwnPresence::default()
        },
        Vec::new(),
    );
    let sync = Rc::new(sync);
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            salt,
            sync: Some(Rc::clone(&sync)),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.handle().replace_tree(desk);
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    Member {
        session,
        sync,
        said,
    }
}

/// What the sharing device's reading says of the window in `tab` .
fn shared(host: &Member, tab: u64) -> Option<Value> {
    let wanted = tab.to_string();
    rows(&shares(&host.session), "sharing")
        .iter()
        .find(|shared| shared["tab"].as_str() == Some(wanted.as_str()))
        .cloned()
}

/// Whether the reading of `host` names one device that asked to see the window in `tab` .
fn asked_for_by_one(host: &Member, tab: u64) -> bool {
    shared(host, tab).is_some_and(|shared| {
        shared["viewers"]
            .as_array()
            .is_some_and(|viewers| viewers.len() == 1)
    })
}

/// What the sharing device's bar is told: each device that asked to see a window, the window, and
/// whether anything of it goes out.
fn told(host: &Member) -> Vec<(u64, u64, bool)> {
    host.session
        .shared
        .window_shares
        .borrow()
        .exposures()
        .iter()
        .map(|viewer| (viewer.peer, viewer.surface, viewer.streaming))
        .collect()
}

/// What the people control on the sharing device's bar tells a screen reader of the shared
/// windows, once it speaks of them.
fn disclosure(host: &Member) -> Option<String> {
    host.session
        .accessible_labels()
        .into_iter()
        .find(|label| label.contains("Show who is here"))
}

/// Pass until the bar's people control says `wanted` of the shared windows, and answer all it
/// says.
#[track_caller]
fn bar_says(
    host: &mut Member,
    window: &mut Option<TestClient>,
    viewer: &mut Member,
    wanted: &str,
) -> String {
    let deadline = Instant::now() + SAID;
    loop {
        settle(host, window, viewer);
        if let Some(said) = disclosure(host).filter(|said| said.contains(wanted)) {
            return said;
        }
        assert!(
            Instant::now() < deadline,
            "the bar's people control never said {wanted:?} of the shared window, it says \
             {:?}\n  the session's labels: {:?}\n  its shares: {}",
            disclosure(host),
            host.session.accessible_labels(),
            shares(&host.session)
        );
    }
}

/// The title of `tab` on `member` 's desk, which is what the bar calls the window in it.
fn title_of(member: &Member, tab: u64) -> String {
    member
        .session
        .handle()
        .read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .find(|shown| shown.id == tab)
                .map(|shown| shown.title.clone())
        })
        .expect("the shared window's tab")
}

/// What would say that someone sees a window: that the screen is visible, that it is viewed, or
/// that anyone is watching.
const SEEN: [&str; 3] = ["visible", "view only", "atching"];

/// A window shared as its page, whose app has handed no page over. The device showing it asked
/// for it and holds nothing of it, and the sharing device's reading says so: nothing goes out
/// (`streaming`), and what the window waits for. The bar is told the device waits, and its
/// people control tells a reader that nothing was sent yet: neither says that anyone watches.
/// Once the app hands its page over and the reading says it goes out, the bar is told the device
/// watches, its people control says the screen is visible to it, and the window's menu is told
/// that the window goes out to the one device, which is when its row counts the device as
/// watching.
#[test]
fn a_device_showing_a_page_that_has_not_gone_out_waits_and_watches_once_it_has() {
    tilo_window_share::flatpak_share::read_encoders();
    let (desk, salt) = desk_of(0x91);
    let mut host = member_minting_under(0x91, desk, salt);
    let mut window = Some(mapped(&mut host.session, APP));
    let tab = tab_of(&host.session, APP);
    // The window's app runs a page engine, and said so when its window opened.
    host.session
        .shared
        .pages
        .borrow_mut()
        .capable(tab, WORKSPACE);
    let mut viewer = member_minting_under(0x92, showing(device(0x91), APP, tab), 1);
    share(&mut host, APP, tab);
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        shared(host, tab).is_some()
    });
    carry_governance(&host.session, &viewer.session);
    heard(&viewer, 0x91, "Framework");

    // The device showing the window asked for it, and the app has handed nothing over.
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        asked_for_by_one(host, tab)
            && shared(host, tab).is_some_and(|shared| {
                shared["waiting_for"]
                    .as_str()
                    .is_some_and(|waits| !waits.is_empty())
            })
    });
    let reading = shared(&host, tab).expect("the window is shared");
    assert_eq!(
        reading["streaming"].as_bool(),
        Some(false),
        "nothing of the page went out: {reading}"
    );
    let asking = viewer.sync.peer();
    assert_eq!(
        told(&host),
        [(asking, tab, false)],
        "the bar is told that the device waits: {reading}"
    );
    let title = title_of(&host, tab);
    let said = bar_says(&mut host, &mut window, &mut viewer, "Nothing sent yet to");
    assert!(
        said.starts_with("Nothing sent yet to ")
            && said.contains(&format!(": waiting for {title}. Show who is here")),
        "a reader is told who waits, and for which window: {said}"
    );
    for seen in SEEN {
        assert!(
            !said.contains(seen),
            "the bar says {seen:?} of a window nothing went out of: {said}"
        );
    }

    // The app hands its page over, as it does while a desk asks for it.
    let page = page_objects();
    let deadline = Instant::now() + SAID;
    let mut handed_at: Option<Instant> = None;
    loop {
        if handed_at.is_none_or(|at| at.elapsed() >= HANDED_EVERY) {
            let mut pages = host.session.shared.pages.borrow_mut();
            for (whole, bytes) in &page {
                pages.received(tab, *whole, bytes.clone());
            }
            drop(pages);
            handed_at = Some(Instant::now());
        }
        settle(&mut host, &mut window, &mut viewer);
        if shared(&host, tab).is_some_and(|shared| shared["streaming"].as_bool() == Some(true)) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the page never went out: {}",
            shares(&host.session)
        );
    }
    assert_eq!(
        told(&host),
        [(asking, tab, true)],
        "the bar is told that the device watches a window that goes out"
    );
    let said = bar_says(&mut host, &mut window, &mut viewer, "Screen visible to");
    assert!(
        said.contains(&format!(": view only {title}. Show who is here"))
            && !said.contains("Nothing sent yet"),
        "a reader is told who sees the window: {said}"
    );
    // What the desk's window menus read of the share, as the session hands it to them.
    let menus = crate::window_share::menu::reading_for_menus(
        &host.session.shared.window_shares,
        Some(&host.sync),
    );
    assert_eq!(
        menus(tab, WORKSPACE),
        Some(WorkspaceShare {
            shared: true,
            viewers: 1,
            streaming: true,
        }),
        "the window's menu is told that the window goes out to the device that asked for it"
    );
}

/// A window shared as pictures by a device whose session is in no live plane of the workspace:
/// its pictures have no way out. A device that asks to see it is named by the share's reading,
/// which says that the window is not sent and what it waits for. The bar is told the device
/// waits, its people control tells a reader that nothing was sent yet, and the window's menu
/// counts the device as waiting: none of them says that anyone watches.
#[test]
fn a_device_asking_for_a_window_whose_pictures_have_no_way_out_waits() {
    tilo_window_share::flatpak_share::read_encoders();
    let (desk, salt) = desk_of(0x93);
    let mut host = member_outside_the_plane(0x93, desk, salt);
    let mut window = Some(mapped(&mut host.session, APP));
    let tab = foreign_tab(&host.session);
    let mut viewer = member(0x94, showing(host.sync.publisher(), APP, tab));
    share(&mut host, APP, tab);
    watch(&mut viewer, tab);
    until(&mut host, &mut window, &mut viewer, |host, _, _| {
        asked_for_by_one(host, tab)
    });
    settle(&mut host, &mut window, &mut viewer);

    let reading = shared(&host, tab).expect("the window is shared");
    assert_eq!(
        reading["streaming"].as_bool(),
        Some(false),
        "a window with no way out is not sent: {reading}"
    );
    assert!(
        reading["waiting_for"]
            .as_str()
            .is_some_and(|waits| !waits.is_empty()),
        "the share says what it waits for: {reading}"
    );
    assert_eq!(
        told(&host),
        [(viewer.sync.peer(), tab, false)],
        "the bar is told that the device waits: {reading}"
    );
    let title = title_of(&host, tab);
    let said = bar_says(&mut host, &mut window, &mut viewer, "Nothing sent yet to");
    assert!(
        said.starts_with("Nothing sent yet to ")
            && said.contains(&format!(": waiting for {title}. Show who is here")),
        "a reader is told who waits, and for which window: {said}"
    );
    for seen in SEEN {
        assert!(
            !said.contains(seen),
            "the bar says {seen:?} of a window nothing went out of: {said}"
        );
    }
    let menu = menu_of(&mut host, tab);
    assert_eq!(
        label_of(&menu, STOP_ROW),
        Some("Stop sharing with workspace (1 waiting)"),
        "{menu:?}"
    );
}

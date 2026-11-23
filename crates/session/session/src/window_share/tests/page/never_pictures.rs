//! A window whose app draws its page is never shared as pictures of it: not when its share is
//! asked for before the page is offered, not under another workspace's name than the one its
//! offer was filed under, and not for the rest of a share that started as pictures before its
//! app said anything. Pictures show a watcher what the page masks.

use super::*;

/// How long a window's first picture may take to be captured, encoded, carried and decoded on a
/// machine busy with the rest of the suite.
const PICTURED: Duration = Duration::from_secs(300);

/// How many passes a share is watched for while it must send nothing: the passes in which a
/// share that started as pictures would have captured, announced and sent its first one.
const HELD: usize = 64;

/// Two devices of one workspace as [`Two`] has them, before anything is shared or offered: the
/// first maps a window of the app, the second lists that window as one that runs on the first.
fn before_any_share(seeds: [u8; 2]) -> Two {
    tilo_window_share::flatpak_share::read_encoders();
    let salt = tilo_desktop_model::ids::device_salt_of_key(device(seeds[0]).as_bytes());
    let mut host = Box::new(member_minting_under(
        seeds[0],
        shell(
            TileContent::LocalApp {
                app_id: APP.to_owned(),
                label: "Notes".to_owned(),
                note: String::new(),
            },
            tilo_desktop_model::ids::compose_id(salt, 1),
        ),
        salt,
    ));
    let window = Some(mapped(&mut host.session, APP));
    // The tab the shell shows the window in: what a share that names nothing else names.
    let tab = foreign_tab(&host.session);
    let tree = host.session.handle().read(Clone::clone);
    assert_eq!(
        crate::window_share::window_shown(host.session.compositor(), &tree, tab)
            .map(|(_, app, workspace)| (app, workspace)),
        Some((APP.to_owned(), WORKSPACE.to_owned())),
        "the tab shows the app's window, in the workspace"
    );
    let viewer = Box::new(member_minting_under(
        seeds[1],
        showing(device(seeds[0]), APP, tab),
        1,
    ));
    Two {
        host,
        window,
        viewer,
        tab,
        page: page_objects(),
        handed_at: None,
        app: None,
    }
}

/// The windows `host`'s live session is asked for pictures of: what a watching shell asks for while no
/// page of the window is announced to it.
fn pictures_wanted(host: &Member) -> Vec<u64> {
    crate::calls::live_sessions_of(&host.sync)
        .get(WORKSPACE)
        .map(|live_session| live_session.wanted_windows())
        .unwrap_or_default()
}

/// The windows `host` captures and encodes pictures of, the ones it shares as their page, and
/// the ones whose share waits to start.
fn shared_how(host: &Member) -> (Vec<u64>, Vec<u64>, Vec<u64>) {
    let shares = host.session.shared.window_shares.borrow();
    (
        shares.hosted.keys().copied().collect(),
        shares.pages.keys().copied().collect(),
        shares.starting.iter().map(|start| start.tab).collect(),
    )
}

/// What `host` says of the share of `tab` that waits to start.
fn starting_row(host: &Member, tab: u64) -> Option<Value> {
    let wanted = tab.to_string();
    rows(&shares(&host.session), "starting")
        .iter()
        .find(|start| start["tab"].as_str() == Some(wanted.as_str()))
        .cloned()
}

/// Whether the watching shell draws the window's page: its tile holds the page, both devices say
/// so, and the page is what the sharing device says it sends.
fn page_drawn(two: &Two) -> bool {
    two.tile()
        .is_some_and(|tile| tile.page && tile.state == StreamState::Pictured)
        && two.received().is_some_and(|stream| {
            stream["pictured"] == true && stream["codec"].as_str() == Some(CODEC)
        })
        && two
            .shared()
            .is_some_and(|shared| shared["streaming"] == true && shared["as_page"] == true)
}

/// The names the watching shell gives what it shows: the text of a page it draws is among them,
/// and a picture of the window names none of it.
fn named_on_the_watching_shell(two: &Two) -> Vec<String> {
    two.viewer
        .session
        .controls()
        .into_iter()
        .map(|control| control.name)
        .collect()
}

/// Whether the watching shell names the words of the page.
fn names_the_page(two: &Two) -> bool {
    let named = named_on_the_watching_shell(two);
    ["Paint stream", "Alpha"]
        .iter()
        .all(|shown| named.iter().any(|name| name.contains(shown)))
}

/// Share the window with the workspace and carry the share to the watching device, which then
/// asks the sharing device for the window.
#[track_caller]
fn share_and_tell(two: &mut Two, host: u8) {
    let tab = two.tab;
    share(&mut two.host, APP, tab);
    assert!(
        two.within(LINK, false, |two| names_the_share(
            &two.host.session,
            two.tab
        )),
        "the workspace does not name the window's share:{}",
        two.said()
    );
    carry_governance(&two.host.session, &two.viewer.session);
    heard(&two.viewer, host, "Framework");
}

/// The window is shared while its app's offer of a page names no window. The app has two windows
/// and says it draws a page on the connection its launcher made, which names no tab, so the
/// session takes the offer up for neither. One of the two is shared then, and a device asks for
/// it. The share waits and says what for: no picture of the window is captured, announced or
/// sent, though the watching device's live session asks for its pictures. When the window names itself
/// the offer is taken up, the share is the page's, and the watching shell draws the page.
///
/// Before, the session decided once, when the share started: no page was offered for the tab, so
/// the window was shared as pictures, and stayed so for the whole share.
#[test]
fn a_window_shared_while_its_apps_offer_names_no_window_waits_for_its_page() {
    const HOST: u8 = 0xa4;
    let mut two = TwoWindows::opened([HOST, 0xa5, 0xa6]);
    let front = two.front;
    let page = page_objects();
    let mut app = App::launched(&two.host.session);
    app.say(&FloatingSurfaceClientMessage::PaintCapable);
    for _ in 0..16 {
        two.pass(&mut [&mut app]);
    }
    assert!(
        two.offered().is_empty(),
        "the offer names no window, and the session took it for the page of {:?}",
        two.offered()
    );

    // Shared, and asked for by a device that is announced no page: it asks for pictures.
    two.share(front);
    assert!(
        two.within(LINK, &mut [&mut app], |two, _| {
            names_the_share(&two.host.session, front)
        }),
        "the workspace does not name the window's share: {}",
        shares(&two.host.session)
    );
    carry_governance(&two.host.session, &two.viewer.session);
    assert!(
        two.within(LINK, &mut [&mut app], |two, _| {
            heard(&two.viewer, HOST, "Framework");
            pictures_wanted(&two.host) == [front]
        }),
        "the watching device never asked for the window: the sharing device's live_session is asked \
         for pictures of {:?}; the watching device says {}",
        pictures_wanted(&two.host),
        shares(&two.viewer.session)
    );

    // It waits: nothing of the window is captured or announced, pass after pass.
    for _ in 0..HELD {
        two.pass(&mut [&mut app]);
        heard(&two.viewer, HOST, "Framework");
        let (pictured, paged, starting) = shared_how(&two.host);
        assert!(
            pictured.is_empty(),
            "pictures of a window whose app draws its page are captured for the workspace: {}",
            shares(&two.host.session)
        );
        assert!(paged.is_empty(), "no page is offered for the window yet");
        assert_eq!(starting, [front], "the share waits to start");
    }
    assert_eq!(
        pictures_wanted(&two.host),
        [front],
        "the watching device still asks for the window's pictures, and is sent none"
    );
    assert!(
        rows(&shares(&two.viewer.session), "received").is_empty(),
        "something of the window reached the watching device: {}",
        shares(&two.viewer.session)
    );
    assert_eq!(
        crate::calls::live_sessions_of(&two.viewer.sync)
            .get(WORKSPACE)
            .map_or(0, |live_session| live_session.heard_total()),
        0,
        "the watching device's live session opened an object of a window that waits for its page"
    );
    let waits = starting_row(&two.host, front).expect("the share that waits");
    assert_eq!(waits["awaits_page"], true, "it waits for its page: {waits}");
    assert_eq!(
        waits["waiting_for"].as_str(),
        Some(tilo_session_core::desktop::i18n::t("session-window-share-page-unoffered").as_str()),
        "and says so in words"
    );
    assert!(
        rows(&shares(&two.host.session), "sharing").is_empty(),
        "a share that waits is not said to be shared: {}",
        shares(&two.host.session)
    );

    // The window names itself: its page is offered, and the share is the page's.
    app.say(&FloatingSurfaceClientMessage::Toolbar {
        toolbar: None,
        instance: Some(front),
    });
    assert!(
        two.within(LINK, &mut [&mut app], |two, _| {
            shared_how(&two.host) == (vec![], vec![front], vec![])
        }),
        "the window named itself as {front}, the session offers the pages of {:?}, and its \
         share is not the page's: {}",
        two.offered(),
        shares(&two.host.session)
    );
    assert!(
        two.within(LINK, &mut [&mut app], |two, apps| {
            heard(&two.viewer, HOST, "Framework");
            apps[0].asked()
        }),
        "the window's app was never asked for its page; the session told it {:?}, and says {}",
        app.told(),
        shares(&two.host.session)
    );
    app.hand_over(&page, Some(front));
    let drawn = |two: &TwoWindows| {
        two.viewer
            .session
            .handle()
            .peer_surface_read(front)
            .is_some_and(|tile| tile.page && tile.state == StreamState::Pictured)
    };
    assert!(
        two.within(LINK, &mut [&mut app], |two, _| {
            heard(&two.viewer, HOST, "Framework");
            drawn(two)
        }),
        "the page was not drawn on the watching shell: the sharing device says {}, the watching \
         one {}",
        shares(&two.host.session),
        shares(&two.viewer.session)
    );
    assert_eq!(
        shared_how(&two.host),
        (vec![], vec![front], vec![]),
        "the window goes out as its page and as nothing else"
    );
}

/// A page offered while its window is shared as pictures takes the share over. The window is
/// shared before its app said anything, by an app this device holds no package of: nothing says
/// it draws a page, so it goes out as pictures and the watching shell decodes them. Then the app
/// says it draws a page. In that pass the pictures end: the sharing device captures the window
/// no longer, shares it as its page and says so, and the watching shell lets the pictures' tile
/// go, asks the live session for the page and draws it, with the page's words on its screen.
///
/// Before, nothing turned a share of pictures into a share of the page: the window went on as
/// pictures until its share was stopped.
#[test]
fn a_page_offered_while_its_window_is_shared_as_pictures_takes_the_share_over() {
    const HOST: u8 = 0xa7;
    let mut two = before_any_share([HOST, 0xa8]);
    let tab = two.tab;
    share_and_tell(&mut two, HOST);
    assert!(
        two.within(PICTURED, false, |two| {
            heard(&two.viewer, HOST, "Framework");
            says_pictured(&two.viewer, two.tab)
        }),
        "the window's pictures did not reach the watching shell:{}",
        two.said()
    );
    assert_eq!(shared_how(&two.host), (vec![tab], vec![], vec![]));
    let pictures = two.received().expect("the pictures' stream");
    assert_ne!(pictures["codec"].as_str(), Some(CODEC));
    assert!(
        two.tile().is_some_and(|tile| !tile.page),
        "the tile decodes pictures"
    );
    assert_eq!(
        two.shared().expect("the share")["as_page"],
        false,
        "the sharing device says it sends pictures"
    );
    assert!(
        !names_the_page(&two),
        "pictures name nothing of the page: {:?}",
        named_on_the_watching_shell(&two)
    );

    // The app says its window draws a page, on its own connection.
    let mut app = App::launched(&two.host.session);
    app.say(&FloatingSurfaceClientMessage::PaintCapable);
    two.app = Some(app);
    assert!(
        two.within(LINK, false, |two| {
            shared_how(&two.host) == (vec![], vec![two.tab], vec![])
        }),
        "the app offered its window's page, and the window is still shared as pictures:{}",
        two.said()
    );
    let shared = two.shared().expect("the page's share");
    assert_eq!(
        shared["as_page"], true,
        "it is shared as its page: {shared}"
    );
    assert_eq!(
        shared["input_allowed"], false,
        "nobody controls a page: {shared}"
    );

    // The watching shell is announced the page over the pictures, and asks the live session for it.
    assert!(
        two.within(LINK, false, |two| {
            heard(&two.viewer, HOST, "Framework");
            two.received()
                .is_some_and(|stream| stream["codec"].as_str() == Some(CODEC))
                && two.asked() == [Track::Page(two.tab)]
                && two.tile().is_some_and(|tile| tile.page)
        }),
        "the page did not take the pictures' place on the watching shell: it asks the live_session for \
         {:?}:{}",
        two.asked(),
        two.said()
    );
    // What it asks the live session for reaches the sharing device's live session through their relay: once
    // it does, nobody asks for the window's pictures any more.
    assert!(
        two.within(LINK, false, |two| {
            heard(&two.viewer, HOST, "Framework");
            pictures_wanted(&two.host).is_empty()
        }),
        "the watching device still asks for the window's pictures: {:?}:{}",
        pictures_wanted(&two.host),
        two.said()
    );

    // The app is asked for the page on its connection, hands it over, and the page is drawn.
    let deadline = Instant::now() + LINK;
    let mut handed: Option<Instant> = None;
    while !(page_drawn(&two) && names_the_page(&two)) {
        assert!(
            Instant::now() < deadline,
            "the page that took the share over was not drawn on the watching shell, which \
             names {:?}; the app was told {:?}:{}",
            named_on_the_watching_shell(&two),
            two.app.as_ref().map(App::told),
            two.said()
        );
        two.pass(false);
        heard(&two.viewer, HOST, "Framework");
        if two.app_asked() && handed.is_none_or(|at| at.elapsed() >= HANDED_EVERY) {
            two.app_hands_over();
            handed = Some(Instant::now());
        }
    }

    // It stays the page's: a picture sent before the page took over, and carried after, draws
    // nothing over it.
    for _ in 0..HELD {
        two.pass(false);
        heard(&two.viewer, HOST, "Framework");
    }
    assert!(
        page_drawn(&two),
        "the page did not stay on the watching shell:{}",
        two.said()
    );
    assert_eq!(shared_how(&two.host), (vec![], vec![tab], vec![]));
    assert_eq!(
        two.viewer.session.handle().peer_surface_tabs(),
        vec![tab],
        "one tile stands for the window, in the tab the shell lists it in"
    );
}

/// A window shared by its tab alone goes out as its page, whatever workspace its app's offer
/// was filed under. Whoever shares at the session's actions names the tab and nothing else, so
/// the workspace is the one whose layout holds the tab. The offer is filed under the workspace
/// the app's launch named, here none, as for an app started in no workspace. The share is what
/// says where the page goes: the window is its page's from its first pass, nothing of it is
/// captured, and the watching shell draws the page.
///
/// Before, a window was shared as its page only if the two names were the same text: any other
/// offer left the window to be shared as pictures.
#[test]
fn a_window_shared_by_its_tab_alone_goes_out_as_its_page_whatever_its_offer_was_filed_under() {
    const HOST: u8 = 0xa9;
    let mut two = before_any_share([HOST, 0xaa]);
    let tab = two.tab;
    two.host.session.shared.pages.borrow_mut().capable(tab, "");
    two.host
        .session
        .invoke(
            SHARES_RESOURCE,
            "share",
            &arguments(serde_json::json!({ "tab": tab.to_string() })),
        )
        .expect("the window is shared by naming its tab");
    assert!(
        two.within(LINK, false, |two| {
            two.shared().is_some() && names_the_share(&two.host.session, two.tab)
        }),
        "the window was not shared:{}",
        two.said()
    );
    assert_eq!(
        shared_how(&two.host),
        (vec![], vec![tab], vec![]),
        "a window whose app offers its page is shared as pictures: {}",
        shares(&two.host.session)
    );
    assert_eq!(
        two.shared().expect("the share")["workspace"].as_str(),
        Some(WORKSPACE),
        "a share that names no workspace is with the one whose layout holds the tab"
    );
    carry_governance(&two.host.session, &two.viewer.session);
    assert!(
        two.within(LINK, true, |two| {
            heard(&two.viewer, HOST, "Framework");
            page_drawn(two)
        }),
        "the page was not drawn on the watching shell:{}",
        two.said()
    );
    assert_eq!(shared_how(&two.host), (vec![], vec![tab], vec![]));
}

/// A window whose app's package says it is its page waits for the page from the first pass of
/// its share: the app has said nothing to the session yet, a device asks for the window's
/// pictures, and none is captured. Once the app offers the page, the share is the page's and the
/// watching shell draws it.
#[test]
fn a_window_whose_package_says_it_is_its_page_waits_for_the_page_before_its_app_spoke() {
    const HOST: u8 = 0xab;
    let mut two = before_any_share([HOST, 0xac]);
    let tab = two.tab;
    two.host
        .session
        .shared
        .window_shares
        .borrow_mut()
        .pages_declared_by(|app| app == APP);
    share_and_tell(&mut two, HOST);
    assert!(
        two.within(LINK, false, |two| {
            heard(&two.viewer, HOST, "Framework");
            pictures_wanted(&two.host) == [two.tab]
        }),
        "the watching device never asked for the window:{}",
        two.said()
    );
    for _ in 0..HELD {
        two.pass(false);
        heard(&two.viewer, HOST, "Framework");
        assert_eq!(
            shared_how(&two.host),
            (vec![], vec![], vec![tab]),
            "the share of a window that is its page does not wait for the page: {}",
            shares(&two.host.session)
        );
    }
    assert!(
        two.received().is_none() && two.opened() == 0,
        "something of the window reached the watching device:{}",
        two.said()
    );
    let waits = starting_row(&two.host, tab).expect("the share that waits");
    assert_eq!(waits["awaits_page"], true, "it waits for its page: {waits}");
    assert_eq!(
        waits["waiting_for"].as_str(),
        Some(tilo_session_core::desktop::i18n::t("session-window-share-page-unoffered").as_str())
    );

    // The app offers its window's page.
    two.host
        .session
        .shared
        .pages
        .borrow_mut()
        .capable(tab, WORKSPACE);
    assert!(
        two.within(LINK, true, |two| {
            heard(&two.viewer, HOST, "Framework");
            page_drawn(two)
        }),
        "the page was not drawn on the watching shell once it was offered:{}",
        two.said()
    );
    assert_eq!(shared_how(&two.host), (vec![], vec![tab], vec![]));
}

/// A window of an app that draws no page is shared as pictures, as it always was, on a device
/// that knows of another app that it draws its windows' pages: by its package, and by what it
/// said to the session.
#[test]
fn a_window_of_an_app_that_draws_no_page_is_shared_as_pictures_beside_one_that_does() {
    const HOST: u8 = 0xad;
    let mut two = before_any_share([HOST, 0xae]);
    let tab = two.tab;
    two.host
        .session
        .shared
        .window_shares
        .borrow_mut()
        .pages_declared_by(|app| app == PHOTOS);
    two.host.session.shared.pages.borrow_mut().engine_of(PHOTOS);
    share_and_tell(&mut two, HOST);
    assert!(
        two.within(PICTURED, false, |two| {
            heard(&two.viewer, HOST, "Framework");
            says_pictured(&two.viewer, two.tab)
        }),
        "the window's pictures did not reach the watching shell:{}",
        two.said()
    );
    assert_eq!(shared_how(&two.host), (vec![tab], vec![], vec![]));
    let shared = two.shared().expect("the share");
    assert_eq!(
        shared["as_page"], false,
        "it goes out as pictures: {shared}"
    );
    assert_eq!(shared["streaming"], true, "and is sent: {shared}");
    assert!(
        two.tile().is_some_and(|tile| !tile.page),
        "the watching shell decodes pictures"
    );
}

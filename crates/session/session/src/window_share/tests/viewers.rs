//! A third session joins the two: two members show the one window a device shares at the same
//! time, each a viewer of its own. Each asks the workspace's live plane for the window's track and
//! acts in the window through a seat of its own, and one that stops showing it ends its own view
//! and nothing of the other's.

use super::super::{VIEW_AGAIN, VIEW_EXPIRY};
use super::*;

/// What the planes carried to one member of a shared window it shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Carried {
    /// The objects the member's live plane opened: the pictures of the one window it asks for.
    pictures: u64,
    /// How often the sharing device told it the window's share ended.
    ends: usize,
}

/// Hand what `from` would have sent on the state plane to each of `to` it was for, as [`carry`]
/// does for one member: a control message to the device it names, and nothing to anyone else. The
/// window's pictures travel the live plane. Answers how often each of `to` was told a window's
/// share ended, in their order.
fn carry_to_each(from: &mut Member, to: &[&Member]) -> Vec<usize> {
    let mut ends = vec![0; to.len()];
    while let Ok(outgoing) = from.said.try_recv() {
        let Cmd::SendControl { ws, msg, target } = outgoing else {
            continue;
        };
        for (member, ended) in to.iter().zip(&mut ends) {
            if target == member.sync.publisher() {
                if matches!(*msg, NeighbourMsg::SurfaceStreamEnd { .. }) {
                    *ended += 1;
                }
                member.sync.hear(said(from, ws, (*msg).clone()));
            }
        }
    }
    ends
}

/// How many objects `member` 's live plane opened in the workspace. A member that shows one shared
/// window asks its plane for that window's track and no other, so these are the window's pictures.
fn pictures_heard(member: &Member) -> u64 {
    crate::calls::planes_of(&member.sync)
        .get(WORKSPACE)
        .map_or(0, |plane| plane.heard_total())
}

/// The tracks `member` asks the workspace's live plane for of the windows `host` shares.
fn tracks_asked(member: &Member, host: &Member) -> Vec<tilo_live::live::Track> {
    member
        .session
        .shared
        .window_shares
        .borrow()
        .shown_tracks
        .get(WORKSPACE)
        .and_then(|hosts| hosts.get(&host.sync.publisher()))
        .map(|tracks| tracks.iter().copied().collect())
        .unwrap_or_default()
}

/// Three devices of one workspace: one shares a window of its desk, and the two others show it.
/// Their live planes meet on the test's relay, which carries the window's pictures; no transport
/// runs between the three state planes either, so every pass carries what each of them said to the
/// device it was for. Each member is kept on the heap: three sessions side by side are more than a
/// test's stack holds.
struct Three {
    /// The two members showing the window.
    viewers: [Box<Member>; 2],
    /// The client whose window the host shares.
    window: Option<TestClient>,
    host: Box<Member>,
    /// How often the host told each viewer the window's share ended, in their order.
    ends: [usize; 2],
    /// The tab the shared window stands in, on all three desks.
    tab: u64,
}

impl Three {
    /// The device of the first seed shares the window of its desk, and the devices of the two
    /// others ask to view it at the same time: both have asked before any of the three takes
    /// another pass.
    fn sharing(seeds: [u8; 3]) -> Self {
        tilo_window_share::flatpak_share::read_encoders();
        let mut host = Box::new(member(seeds[0], own_desk()));
        let window = Some(mapped(&mut host.session, APP));
        let tab = tab_of(&host.session, APP);
        share(&mut host, APP, tab);
        let mut viewers = [seeds[1], seeds[2]]
            .map(|seed| Box::new(member(seed, showing(host.sync.publisher(), APP, tab))));
        for viewer in &mut viewers {
            watch(viewer, tab);
        }
        Self {
            viewers,
            window,
            host,
            ends: [0; 2],
            tab,
        }
    }

    /// One pass of the client and of each of the three sessions, with what their planes said
    /// carried after it.
    fn pass(&mut self) {
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
        for viewer in &mut self.viewers {
            viewer.session.step(Duration::from_millis(2)).expect("step");
        }
        self.carry();
    }

    /// Carry what each of the three said to the device it was for, counting how often the host
    /// told each viewer the window's share ended.
    fn carry(&mut self) {
        let [first, second] = &mut self.viewers;
        let ended = carry_to_each(&mut self.host, &[&**first, &**second]);
        for (kept, ended) in self.ends.iter_mut().zip(ended) {
            *kept += ended;
        }
        carry_to_each(first, &[&*self.host, &**second]);
        carry_to_each(second, &[&*self.host, &**first]);
    }

    /// What the planes carried to each viewer so far, in their order: the window's pictures its
    /// live plane opened, and the ends of the share it was told.
    fn carried(&self) -> [Carried; 2] {
        [0, 1].map(|at| Carried {
            pictures: pictures_heard(&self.viewers[at]),
            ends: self.ends[at],
        })
    }

    /// Pass until `done` holds of the three, as [`until`] does of two.
    #[track_caller]
    fn until(&mut self, done: impl FnMut(&Self) -> bool) {
        // A window is captured, encoded and decoded twice over here: on a machine busy with the
        // rest of the suite that takes far longer than it does alone.
        self.within(Duration::from_secs(300), done);
    }

    /// Pass until `done` holds of the three, which it must within `limit` .
    #[track_caller]
    fn within(&mut self, limit: Duration, mut done: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + limit;
        loop {
            self.pass();
            if done(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "not reached in time:\n  host: {}\n  first: {}\n  second: {}\n  carried: {:?}",
                shares(&self.host.session),
                shares(&self.viewers[0].session),
                shares(&self.viewers[1].session),
                self.carried()
            );
        }
    }

    /// Pass for `time` : what the three do of their own accord while it goes by.
    fn pass_for(&mut self, time: Duration) {
        let end = Instant::now() + time;
        while Instant::now() < end {
            self.pass();
        }
    }

    /// Pass a few times without asking for anything.
    fn settle(&mut self) {
        for _ in 0..8 {
            self.pass();
        }
    }

    /// Both members picture the window where they show it, and the sharing device names both as
    /// viewing it.
    #[track_caller]
    fn shown_to_both(&mut self) {
        let tab = self.tab;
        self.until(|three| {
            three.viewers.iter().all(|viewer| pictured(viewer, tab))
                && watchers(&three.host).len() == 2
        });
    }

    /// The person sharing the window lets the members viewing it control it, and the sharing
    /// device says both may.
    #[track_caller]
    fn let_both_in(&mut self) {
        allow_input(&mut self.host, self.tab, true);
        self.until(|three| {
            let watching = watchers(&three.host);
            watching.len() == 2 && watching.iter().all(|(_, can_control)| *can_control)
        });
    }

    /// What the shared window's client saw.
    #[track_caller]
    fn seen(&self) -> &Seen {
        &self
            .window
            .as_ref()
            .expect("the shared window's client")
            .seen
    }
}

/// Whether `viewer` 's tile of the window shown in `tab` holds a picture decoded from its stream.
fn pictured(viewer: &Member, tab: u64) -> bool {
    viewer
        .session
        .handle()
        .peer_surface_read(tab)
        .is_some_and(|read| read.pictured())
}

/// Who the sharing device's own account says views its one shared window, each with whether it may
/// control it, in the order the account names them: nobody while it shares nothing.
fn watchers(host: &Member) -> Vec<(u64, bool)> {
    let reading = shares(&host.session);
    rows(&reading, "sharing")
        .first()
        .and_then(|shared| shared["viewers"].as_array())
        .map(|viewers| {
            viewers
                .iter()
                .filter_map(|viewer| {
                    Some((viewer["peer"].as_u64()?, viewer["can_control"].as_bool()?))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `members` in the order the shares keep them: by handle.
fn in_order<T: Ord>(mut members: Vec<T>) -> Vec<T> {
    members.sort_unstable();
    members
}

/// The seats the members controlling `host` 's shared windows act at, by member and window: how the
/// shares key the seat they open for a viewer.
fn seats(
    host: &Member,
) -> BTreeMap<(neighbour_wire::PeerId, neighbour_wire::SurfaceId), tilo_compositor::Seat> {
    host.session.shared.window_shares.borrow().seats.clone()
}

/// Stop showing the window another member shares, in `tab` of the workspace.
fn unwatch(member: &mut Member, tab: u64) {
    member
        .session
        .invoke(
            SHARES_RESOURCE,
            "unwatch",
            &arguments(serde_json::json!({ "tab": tab.to_string(), "room": WORKSPACE })),
        )
        .expect("the window is no longer asked for");
}

/// One session shares a window of its desk and two others ask to view it at the same time: each
/// asks the workspace's live plane for the window's track and is carried its pictures as a viewer
/// of its own, each tab holds the picture decoded from them, and the sharing device's account and
/// its bar name both.
#[test]
fn a_shared_window_reaches_both_members_showing_it() {
    let mut three = Three::sharing([0x31, 0x32, 0x33]);
    let tab = three.tab;
    three.shown_to_both();

    let sharer = neighbour_wire::peer_handle(three.host.sync.publisher().as_bytes());
    let [first, second] = [0, 1].map(|at| three.viewers[at].sync.peer());
    assert_ne!(
        first, second,
        "the two members are two viewers, each with a handle of its own"
    );
    for (viewer, carried) in three.viewers.iter().zip(three.carried()) {
        // The live plane carries a window's pictures only to the planes that ask for its track:
        // what was counted here this member's own plane asked for and opened.
        assert_eq!(
            tracks_asked(viewer, &three.host),
            [tilo_live::live::Track::Window(
                tab,
                tilo_live::video::Layer::High
            )],
            "the member asks the plane for the window's pictures, as a viewer of its own"
        );
        assert!(
            carried.pictures > 0,
            "the window's pictures were carried to the member's own plane: {carried:?}"
        );

        let reading = shares(&viewer.session);
        let received = rows(&reading, "received");
        assert_eq!(
            received.len(),
            1,
            "the one shared window arrived: {reading}"
        );
        let stream = &received[0];
        assert_eq!(
            stream["app_id"].as_str(),
            Some(APP),
            "what arrived: {reading}"
        );
        assert_eq!(
            stream["from"].as_u64(),
            Some(sharer),
            "the member that shares it: {reading}"
        );
        assert!(
            stream["codec"]
                .as_str()
                .is_some_and(|codec| !codec.is_empty()),
            "the stream says how it is encoded: {reading}"
        );
        assert!(
            stream["pictured"].as_bool() == Some(true),
            "the tab shows the picture it decoded: {reading}"
        );
        assert!(
            stream["width"].as_u64().unwrap_or(0) > 0 && stream["height"].as_u64().unwrap_or(0) > 0,
            "the stream says how big the window is: {reading}"
        );

        // The tile of the tab the window stands in decodes what its sender streams, on each of the
        // two desks.
        let read = viewer
            .session
            .handle()
            .peer_surface_read(tab)
            .expect("the shared window has a tile");
        assert_eq!(read.tab, tab, "the tab it stands in");
        assert_eq!(read.surface, tab, "the surface its sender names");
        assert_eq!(read.app_id, APP, "the window it shows");
        assert_eq!(read.peer, sharer, "the device sharing it");
        assert_eq!(
            read.standing,
            tilo_desktop_surface::Standing::Pictured,
            "the tile reads as a picture of the window"
        );
        let (width, height, bytes) = read.picture.expect("the picture the tile decoded");
        assert!(
            width > 0 && height > 0,
            "the picture keeps the sender's size"
        );
        assert_eq!(
            bytes,
            usize::try_from(width).unwrap() * usize::try_from(height).unwrap() * 4,
            "the picture is RGBA at that size"
        );
    }

    // The sharing device's own account names both members, each read only until the person lets
    // it in.
    let reading = shares(&three.host.session);
    let sharing = rows(&reading, "sharing");
    assert_eq!(sharing.len(), 1, "the one window is shared: {reading}");
    assert_eq!(
        sharing[0]["tab"].as_str(),
        Some(tab.to_string().as_str()),
        "which window: {reading}"
    );
    let both = in_order(vec![(first, false), (second, false)]);
    assert_eq!(
        watchers(&three.host),
        both,
        "both members watch it, read only until the person allows control: {reading}"
    );
    assert_eq!(
        sharing[0]["input_allowed"].as_bool(),
        Some(false),
        "nobody was let in: {reading}"
    );
    let exposures = three.host.session.shared.window_shares.borrow().exposures();
    assert_eq!(
        exposures,
        both.iter()
            .map(|(peer, can_control)| tilo_desktop::chrome::ActiveExposure {
                peer: *peer,
                surface: tab,
                can_control: *can_control,
                name: String::new(),
            })
            .collect::<Vec<_>>(),
        "the bar's sharing indicator says both are watching the window"
    );
}

/// Two members control a window a third device shares: each one's pointer and keys reach the
/// window, and each acts at a seat of its own, with its own pointer, beside the other's.
#[test]
fn two_members_reach_the_shared_window_each_through_a_seat_of_its_own() {
    let mut three = Three::sharing([0x34, 0x35, 0x36]);
    let tab = three.tab;
    three.shown_to_both();
    let [first, second] = [0, 1].map(|at| three.viewers[at].sync.peer());
    // Each member's pointer is in the stream's own pixels: the first up and left of the middle of
    // the picture the host sends, the second down and right of it.
    let centre = received_centre(&three.viewers[0]);
    let places = [
        (centre.0 - 40.0, centre.1 - 40.0),
        (centre.0 + 40.0, centre.1 + 40.0),
    ];

    for (viewer, place) in three.viewers.iter().zip(places) {
        move_pointer(viewer, &three.host, tab, place.0, place.1);
    }
    three.settle();
    assert!(
        !three.seen().pointer_inside && three.seen().enters == 0 && three.seen().motions.is_empty(),
        "members the person did not let in move nothing"
    );
    assert!(
        seats(&three.host).is_empty(),
        "no seat is opened for a member the person did not let in"
    );

    three.let_both_in();

    // The first member's pointer comes over the window: it acts at a seat, and the member that has
    // not acted yet has none.
    move_pointer(
        &three.viewers[0],
        &three.host,
        tab,
        places[0].0,
        places[0].1,
    );
    three.until(|three| three.seen().enters > 0);
    let opened = seats(&three.host);
    assert_eq!(
        opened.keys().copied().collect::<Vec<_>>(),
        vec![(first, tab)],
        "the seat is the one of the member that moved, in the window it controls"
    );
    let first_seat = opened[&(first, tab)];
    let first_at = three
        .host
        .session
        .compositor()
        .seat_pointer(first_seat)
        .expect("the seat's pointer is where its member put it");

    // The second member's pointer comes over the window too: it is given a seat of its own, and
    // the first member's is left as it was.
    let entered = three.seen().enters;
    move_pointer(
        &three.viewers[1],
        &three.host,
        tab,
        places[1].0,
        places[1].1,
    );
    three.until(|three| three.seen().enters > entered);
    let opened = seats(&three.host);
    assert_eq!(
        opened.keys().copied().collect::<Vec<_>>(),
        in_order(vec![(first, tab), (second, tab)]),
        "each member has a seat under its own handle"
    );
    assert_eq!(
        opened[&(first, tab)],
        first_seat,
        "the first member's seat is still its own: the second took none of it"
    );
    let second_seat = opened[&(second, tab)];
    assert_ne!(
        first_seat, second_seat,
        "the two members do not act at one seat"
    );
    let shared = identifier_of(three.host.session.compositor(), APP);
    for seat in [first_seat, second_seat] {
        let aim = three
            .host
            .session
            .compositor()
            .seat_view(seat)
            .expect("the seat is open");
        assert_eq!(
            aim.window.map(|window| window.identifier),
            Some(shared.clone()),
            "each seat acts in the shared window"
        );
    }
    let second_at = three
        .host
        .session
        .compositor()
        .seat_pointer(second_seat)
        .expect("the seat's pointer is where its member put it");
    assert_eq!(
        three.host.session.compositor().seat_pointer(first_seat),
        Some(first_at),
        "the first member's pointer stays where it left it while the second moves its own"
    );
    assert!(
        second_at.0 > first_at.0 && second_at.1 > first_at.1,
        "each seat's pointer is where its own member put it: {first_at:?} and {second_at:?}"
    );

    // Once over the window, each member's move moves the window's own pointer, from where that
    // member left it to where that member put it.
    let moved = three.seen().motions.len();
    move_pointer(
        &three.viewers[0],
        &three.host,
        tab,
        places[0].0 + 10.0,
        places[0].1 + 10.0,
    );
    three.until(|three| three.seen().motions.len() > moved);
    let first_moved = *three.seen().motions.last().expect("the pointer moved");
    let moved = three.seen().motions.len();
    move_pointer(
        &three.viewers[1],
        &three.host,
        tab,
        places[1].0 + 10.0,
        places[1].1 + 10.0,
    );
    three.until(|three| three.seen().motions.len() > moved);
    let second_moved = *three.seen().motions.last().expect("the pointer moved");
    assert!(
        second_moved.0 > first_moved.0 && second_moved.1 > first_moved.1,
        "the window's pointer went where each member moved its own: {first_moved:?} and \
         {second_moved:?}"
    );

    // Each member's keyboard reaches the window through its seat: one press and its release from
    // each, in the order they typed.
    let heard = three.seen().keys.len();
    for (viewer, key) in three.viewers.iter().zip([30, 31]) {
        for pressed in [true, false] {
            send_input(
                viewer,
                &three.host,
                tab,
                &SharedInput::Key {
                    keysym: key,
                    pressed,
                    modifiers: 0,
                },
            );
        }
    }
    three.until(|three| three.seen().keys.len() >= heard + 4);
    assert_eq!(
        three.seen().keys[heard..],
        [(30, true), (30, false), (31, true), (31, false)],
        "each member's keystroke reached the window as one press and its release"
    );

    assert_eq!(
        seats(&three.host),
        opened,
        "each member kept the one seat it opened: none was opened again, and none taken over"
    );
}

/// Of two members showing a window a third device shares, one stops: that member's view ends
/// alone. It asks the plane for the window no longer and is carried nothing more of it, and the
/// sharing device names only the other, which goes on being carried the window's pictures.
#[test]
fn a_member_that_stops_showing_a_shared_window_ends_its_own_stream_only() {
    let mut three = Three::sharing([0x37, 0x38, 0x39]);
    let tab = three.tab;
    three.shown_to_both();
    let [staying, leaving] = [0, 1].map(|at| three.viewers[at].sync.peer());
    assert_eq!(
        watchers(&three.host),
        in_order(vec![(staying, false), (leaving, false)]),
        "the sharing device names both members while both show the window"
    );

    // The second member stops showing the window. Nothing tells the sharing device so: the member
    // only stops asking for it, as a device that went away does, and a viewer's ask keeps its
    // stream up for no longer than the expiry.
    unwatch(&mut three.viewers[1], tab);
    three.within(VIEW_EXPIRY * 10, |three| {
        watchers(&three.host) == [(staying, false)]
    });
    assert!(
        rows(&shares(&three.viewers[1].session), "watching").is_empty(),
        "the member that stopped asks for nothing"
    );
    let exposures = three.host.session.shared.window_shares.borrow().exposures();
    assert_eq!(
        exposures,
        vec![tilo_desktop::chrome::ActiveExposure {
            peer: staying,
            surface: tab,
            can_control: false,
            name: String::new(),
        }],
        "the bar's sharing indicator names the one member still watching"
    );

    // The member still showing the window says so again every few seconds. Two of those asks go
    // by, so whatever the sharing device makes of the other's leaving it has made by now; then the
    // window's pictures are still coming to the member that stayed, and its tab still pictures it.
    let left = three.carried();
    three.pass_for(VIEW_AGAIN * 2);
    let later = three.carried()[0].pictures;
    three.until(|three| {
        three.carried()[0].pictures >= later + 10 && pictured(&three.viewers[0], tab)
    });
    assert!(
        tracks_asked(&three.viewers[1], &three.host).is_empty(),
        "the member that stopped showing the window asks the plane for none of it"
    );
    assert_eq!(
        three.carried()[1],
        left[1],
        "nothing more of the window is carried to the member that stopped showing it"
    );
    assert_eq!(
        watchers(&three.host),
        [(staying, false)],
        "the sharing device goes on naming the one member that shows the window"
    );
    assert_eq!(
        three.carried()[0].ends,
        left[0].ends,
        "the member still showing the window was not told its share ended with the other's view"
    );
}

/// A member that controlled a shared window and stopped showing it has no hand left in it: what
/// it still sends moves nothing and its seat closes with its view, while the member still
/// showing the window keeps its own seat and its reach.
#[test]
fn a_member_that_stopped_showing_a_shared_window_no_longer_reaches_it() {
    let mut three = Three::sharing([0x3a, 0x3b, 0x3c]);
    let tab = three.tab;
    three.shown_to_both();
    let [staying, leaving] = [0, 1].map(|at| three.viewers[at].sync.peer());
    three.let_both_in();
    let centre = received_centre(&three.viewers[0]);
    let places = [
        (centre.0 - 40.0, centre.1 - 40.0),
        (centre.0 + 40.0, centre.1 + 40.0),
    ];

    // Both members act in the window, so each holds a seat there.
    for (viewer, place) in [0, 1].into_iter().zip(places) {
        let entered = three.seen().enters;
        move_pointer(&three.viewers[viewer], &three.host, tab, place.0, place.1);
        three.until(|three| three.seen().enters > entered);
    }
    let opened = seats(&three.host);
    assert_eq!(
        opened.keys().copied().collect::<Vec<_>>(),
        in_order(vec![(staying, tab), (leaving, tab)]),
        "each member has a seat under its own handle"
    );

    // The second member stops showing the window, and the sharing device stops hearing it ask.
    unwatch(&mut three.viewers[1], tab);
    three.within(VIEW_EXPIRY * 10, |three| {
        watchers(&three.host) == [(staying, true)]
    });

    // What the member that left still sends reaches nothing: the person let it in while it showed
    // the window, and the sharing device no longer names it as showing it.
    let (entered, moved) = (three.seen().enters, three.seen().motions.len());
    move_pointer(
        &three.viewers[1],
        &three.host,
        tab,
        places[1].0 + 10.0,
        places[1].1 + 10.0,
    );
    three.settle();
    assert_eq!(
        (three.seen().enters, three.seen().motions.len()),
        (entered, moved),
        "a member that stopped showing the window moves nothing in it"
    );
    // Its seat closed with its view and none was opened for it again, and the other member's
    // seat is its own still.
    let kept = BTreeMap::from([((staying, tab), opened[&(staying, tab)])]);
    assert_eq!(
        seats(&three.host),
        kept,
        "the one seat left is the one of the member still showing the window"
    );

    // The member still showing it reaches the window as before, at the seat it had.
    three.until(|three| watchers(&three.host) == [(staying, true)]);
    let entered = three.seen().enters;
    move_pointer(
        &three.viewers[0],
        &three.host,
        tab,
        places[0].0 + 10.0,
        places[0].1 + 10.0,
    );
    three.until(|three| three.seen().enters > entered);
    assert_eq!(
        seats(&three.host),
        kept,
        "the member still showing the window acts at the seat it had"
    );
}

/// The person's own other device views a window this one shares: the bar's sharing indicator names
/// it by what that device calls itself, as the certificate it said names it, and never by its
/// handle.
#[test]
fn the_persons_own_other_device_viewing_a_window_goes_by_its_device_name() {
    tilo_window_share::flatpak_share::read_encoders();
    // Both devices are the one person's: the same root signed the certificate each says.
    let own = person(0x58);
    let salt = Options::default().salt;
    let mut host = member_as(
        0x58,
        governed(own_desk(), &own, &[]),
        salt,
        Some(own.clone()),
    );
    let mut window = Some(mapped(&mut host.session, APP));
    let tab = tab_of(&host.session, APP);
    share(&mut host, APP, tab);
    let mut viewer = member_as(
        0x59,
        governed(showing(host.sync.publisher(), APP, tab), &own, &[]),
        salt,
        Some(own),
    );
    heard_of(&host, 0x59, 0x58, "Phone");
    heard_of(&viewer, 0x58, 0x58, "Framework");

    watch(&mut viewer, tab);
    viewing(&mut host, &mut window, &mut viewer, tab);

    let exposures = host.session.shared.window_shares.borrow().exposures();
    assert_eq!(
        exposures,
        vec![tilo_desktop::chrome::ActiveExposure {
            peer: viewer.sync.peer(),
            surface: tab,
            can_control: false,
            name: "Phone".to_owned(),
        }],
        "the bar names the person's own other device by what it calls itself"
    );
}

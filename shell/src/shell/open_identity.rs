//! When two arrivals are one open.
//!
//! Two mechanisms can put an application on this desktop because somebody
//! else asked for it, and for a while both did:
//!
//! * [`crate::shell::agent_opens`] — a participant asked, over this
//!   session's collaboration socket or over the room
//!   (`NeighbourMsg::AppOpened`). This is the one that carries the
//!   *content* an open was bound to, so it is the one that can open the
//!   photo editor on the photograph.
//! * [`crate::shell::peer_windows`] — a tab arrived in the room document
//!   naming an app, and every member starts a local instance of it.
//!
//! Neither knew about the other, so one request produced a window per
//! mechanism per session — and each of those windows was itself projected
//! into the room, where every other member read it as news and opened one
//! more. One person asking for the photo editor once left three tabs
//! called "Photos" on somebody else's screen.
//!
//! This is the one place both of them ask "have I already done this", so
//! that the second arrival of an open is recognised as the same thing
//! rather than acted on as a new one.
//!
//! # Two questions, because the two can see different things
//!
//! An open either names content or it does not, and the two mechanisms do
//! not see the same amount of it. `AppOpened` carries the content hash a
//! request was bound to. A tab in the room document carries none —
//! `WorkspaceTree::project_owned_contents` writes a tab's existence, the
//! app that draws it and its title, and deliberately nothing else. So
//! each asks the question it is able to answer:
//!
//! * [`realised`] — *is this the same open*: the same app, in the same
//!   workspace, bound to the same content. What [`crate::shell::agent_opens`]
//!   asks, because it can tell, and because an open naming a photograph is
//!   not answered by a window that does not hold it.
//! * [`realised_in_room`] — *does this room's window of this app already
//!   exist here*: app and room, content left out. What
//!   [`crate::shell::peer_windows`] asks, because that is the whole of what
//!   a projected tab says, and a second window of one app for one room is
//!   the thing it exists to avoid.
//!
//! That asymmetry is what "the open that names content wins" means in
//! code. The content open still happens; the projected tab yields to it.
//!
//! # First claim wins
//!
//! Both callers run on the thread that draws — `agent_opens::pump` and
//! `peer_windows::examine` are both frame passes — so a check and the
//! claim after it cannot interleave, and a request arriving by both routes
//! at once resolves to whichever pass runs first. In practice that is the
//! frame that carried it: `AppOpened` crosses on gossip in milliseconds,
//! where a projected tab waits for the room's next sync round.
//!
//! # Tabs the room already knows about
//!
//! An open that arrived over the collaboration socket is announced to the
//! room by the frame that carries it, and `NeighbourMsg::AppOpened` says
//! in as many words that a session must never republish one. Projecting
//! the tab it produced *is* republishing it: every other member's
//! `peer_windows::examine` reads a tab in that map as news that the room
//! has a window open and starts one of its own for it. That is where the
//! second and third windows came from, and it is why such a tab is
//! recorded here and left out of the projection — see
//! [`announced_by_frame`], which `project_owned_contents` asks.
//!
//! The tab an ordinary launch produces is untouched by that and projects
//! as it always did, which is what keeps a workspace carrying its windows.

use std::sync::Mutex;

use crate::shell::state::{RoomId, TabId};

/// One open: an application, a workspace, and the content it was bound to.
///
/// See the module header for why one of the two callers matches on all
/// three of these and the other matches on the first two.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenIdentity {
    /// The application's package id, folded to lower case: the two
    /// mechanisms read it from different places — a wire field and a tab's
    /// tile — and neither promises a case.
    app: String,
    /// The room, as its uuid. Empty for an open in no room, which is every
    /// open on a session collaborating with nobody.
    room: String,
    /// The content the open was bound to, as a blake3 hex, or `None` for an
    /// open that named no object.
    with: Option<String>,
}

impl OpenIdentity {
    /// The open of `app_id` onto `with`, in the room `room` names.
    ///
    /// `room` is a cluster id as an announcement carries it, normalised
    /// through [`RoomId`] so the string a peer sent and the string a
    /// workspace holds are one string.
    pub fn new(app_id: &str, room: &str, with: Option<&str>) -> Self {
        let room = RoomId::parse(room.trim())
            .filter(|r| !r.is_nil())
            .map(|r| r.to_string())
            .unwrap_or_default();
        Self {
            app: normalise(app_id),
            room,
            with: with
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty()),
        }
    }

    /// The open of `app_id` onto `with` in `room`, for a caller that
    /// already holds a workspace's own room id.
    pub fn in_room(app_id: &str, room: RoomId, with: Option<&str>) -> Self {
        Self {
            app: normalise(app_id),
            room: room_key(room),
            with: with
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty()),
        }
    }

    /// Whether this names an application at all. An open of nothing is not
    /// an open, and must not collapse with another one.
    pub fn is_named(&self) -> bool {
        !self.app.is_empty()
    }
}

/// The one spelling of a package id this module compares.
fn normalise(app_id: &str) -> String {
    app_id.trim().to_ascii_lowercase()
}

/// The one spelling of a room this module compares. A room that has not
/// been minted yet is no room, exactly as `CrdtRoute` treats the nil uuid.
fn room_key(room: RoomId) -> String {
    if room.is_nil() {
        String::new()
    } else {
        room.to_string()
    }
}

/// The opens this session has produced a window for, and the tab each one
/// lives in.
///
/// A `Vec` behind a `static Mutex` rather than something threaded through
/// both callers: this has the shape and the lifetime of
/// `launcher::PENDING_PLACEMENT`, which correlates a launch with the
/// window it eventually produces. There are a handful of entries in a
/// session and both readers run once a frame.
static REALISED: Mutex<Vec<(OpenIdentity, TabId)>> = Mutex::new(Vec::new());

/// Tabs whose open the room learns of from `NeighbourMsg::AppOpened`, so
/// the projection must not announce them a second time. See the module
/// header.
static ANNOUNCED_BY_FRAME: Mutex<Vec<TabId>> = Mutex::new(Vec::new());

/// The tab this session already produced for exactly this open, if any.
///
/// `Some` is the whole answer to "has this been acted on here": the tab
/// exists from the moment the launch is started, before its window
/// arrives, which is exactly the interval the other mechanism used to slip
/// through.
pub fn realised(id: &OpenIdentity) -> Option<TabId> {
    if !id.is_named() {
        return None;
    }
    REALISED
        .lock()
        .expect("REALISED poisoned")
        .iter()
        .find(|(known, _)| known == id)
        .map(|(_, tab)| *tab)
}

/// The tab this session already produced for *some* open of `app_id` in
/// `room`, whatever content it named.
///
/// The coarser of the two questions, and the one a projected tab can ask:
/// the room document names no content, so "the room has this app open and
/// I have a window for it" is as fine a match as the tab is able to make.
pub fn realised_in_room(app_id: &str, room: RoomId) -> Option<TabId> {
    let app = normalise(app_id);
    if app.is_empty() {
        return None;
    }
    let room = room_key(room);
    REALISED
        .lock()
        .expect("REALISED poisoned")
        .iter()
        .find(|(known, _)| known.app == app && known.room == room)
        .map(|(_, tab)| *tab)
}

/// Record that this session is opening `id`, in `tab`.
///
/// `false` means one was already recorded and the caller must not open a
/// window: what it is holding is the request that produced the tab already
/// there, arriving by the other route.
///
/// Check and record in one call rather than a [`realised`] followed by a
/// write, so there is no gap for the second arrival to pass through.
pub fn claim(id: OpenIdentity, tab: TabId) -> bool {
    if !id.is_named() {
        return true;
    }
    let mut realised = REALISED.lock().expect("REALISED poisoned");
    if realised.iter().any(|(known, _)| *known == id) {
        return false;
    }
    realised.push((id, tab));
    true
}

/// Forget `id` — the launch it was claimed for never started, so nothing
/// came of it and the next ask has to be able to try again.
pub fn release(id: &OpenIdentity) {
    let mut realised = REALISED.lock().expect("REALISED poisoned");
    let Some(pos) = realised.iter().position(|(known, _)| known == id) else {
        return;
    };
    let (_, tab) = realised.remove(pos);
    drop(realised);
    forget_tab(tab);
}

/// Record that `tab` holds an open the room was told about by the frame
/// that carried it.
///
/// Read by `WorkspaceTree::project_owned_contents`, which leaves such a
/// tab out of the room document. Announcing it there as well is what made
/// every member open a second window for an open they had already been
/// told about.
pub fn announce_by_frame(tab: TabId) {
    let mut announced = ANNOUNCED_BY_FRAME
        .lock()
        .expect("ANNOUNCED_BY_FRAME poisoned");
    if !announced.contains(&tab) {
        announced.push(tab);
    }
}

/// Whether the room already knows about `tab`'s open from the frame that
/// carried it.
pub fn announced_by_frame(tab: TabId) -> bool {
    ANNOUNCED_BY_FRAME
        .lock()
        .expect("ANNOUNCED_BY_FRAME poisoned")
        .contains(&tab)
}

/// Drop `tab` from the set the projection holds back, for a tab that never
/// came to anything.
pub fn forget_tab(tab: TabId) {
    ANNOUNCED_BY_FRAME
        .lock()
        .expect("ANNOUNCED_BY_FRAME poisoned")
        .retain(|t| *t != tab);
}

/// Drop everything. Tests share this process-wide state exactly as they
/// share the launcher's.
#[cfg(test)]
pub fn clear_for_test() {
    REALISED.lock().expect("REALISED poisoned").clear();
    ANNOUNCED_BY_FRAME
        .lock()
        .expect("ANNOUNCED_BY_FRAME poisoned")
        .clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHOTOS: &str = "dev.qos.photos";
    const ROOM: &str = "b6385eec-3432-411f-a661-fb6f5d1b0d27";
    const PICTURE: &str = "623fdbf0c19749dd";

    /// The second arrival of one open is refused a window. This is the
    /// whole contract.
    #[test]
    #[serial_test::serial(process_env)]
    fn one_open_is_claimed_once() {
        clear_for_test();
        assert!(
            claim(OpenIdentity::new(PHOTOS, ROOM, Some(PICTURE)), 11),
            "the first arrival opens the window"
        );
        assert!(
            !claim(OpenIdentity::new(PHOTOS, ROOM, Some(PICTURE)), 22),
            "the second arrival of the same open must not open another"
        );
        assert_eq!(
            realised(&OpenIdentity::new(PHOTOS, ROOM, Some(PICTURE))),
            Some(11),
            "and `where is it` stays the tab the first arrival made"
        );
    }

    /// The coarse question is the one a projected tab can ask: it names an
    /// app and a room and no content at all, so a window opened onto a
    /// photograph still answers it.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_projected_tab_matches_an_open_that_named_content() {
        clear_for_test();
        let room = RoomId::parse(ROOM).expect("the fixture is a uuid");
        assert_eq!(realised_in_room(PHOTOS, room), None);
        assert!(claim(OpenIdentity::new(PHOTOS, ROOM, Some(PICTURE)), 11));
        assert_eq!(
            realised_in_room(PHOTOS, room),
            Some(11),
            "the room document names no content, so the window that opened \
             onto one is still this room's window of the app"
        );
        assert_eq!(
            realised_in_room("dev.qos.terminal", room),
            None,
            "a different app is a different window"
        );
    }

    /// The exact question keeps them apart, which is what lets an open
    /// naming a photograph go ahead when the window already there does not
    /// hold it.
    #[test]
    #[serial_test::serial(process_env)]
    fn content_makes_a_different_open() {
        clear_for_test();
        assert!(claim(OpenIdentity::new(PHOTOS, ROOM, None), 1));
        assert!(
            claim(OpenIdentity::new(PHOTOS, ROOM, Some(PICTURE)), 2),
            "an open onto a picture is not answered by a window that has none"
        );
        assert!(
            !claim(OpenIdentity::new(PHOTOS, ROOM, Some(PICTURE)), 3),
            "but a second arrival of that one still is"
        );
    }

    /// The case a package id reaches this in differs by route — one reads a
    /// wire field, the other a tab's tile — so it must not make two opens
    /// out of one.
    #[test]
    #[serial_test::serial(process_env)]
    fn case_does_not_make_a_second_open() {
        clear_for_test();
        let room = RoomId::parse(ROOM).expect("the fixture is a uuid");
        assert!(claim(OpenIdentity::new("Dev.QOS.Photos", ROOM, None), 1));
        assert!(!claim(OpenIdentity::new(PHOTOS, ROOM, None), 2));
        assert_eq!(realised_in_room("DEV.QOS.PHOTOS", room), Some(1));
    }

    /// The room is half the identity. The same app in another room is
    /// another open and gets its own window.
    #[test]
    #[serial_test::serial(process_env)]
    fn another_room_is_another_open() {
        clear_for_test();
        let elsewhere = "11111111-1111-4111-8111-111111111111";
        assert!(claim(OpenIdentity::new(PHOTOS, ROOM, None), 1));
        assert!(
            claim(OpenIdentity::new(PHOTOS, elsewhere, None), 2),
            "a workspace does not inherit another workspace's windows"
        );
        assert_eq!(
            realised_in_room(PHOTOS, RoomId::parse(elsewhere).unwrap()),
            Some(2)
        );
    }

    /// A launch that never started leaves nothing behind, or the app could
    /// not be opened again for the rest of the session.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_released_claim_can_be_made_again() {
        clear_for_test();
        let id = OpenIdentity::new(PHOTOS, ROOM, None);
        assert!(claim(id.clone(), 7));
        announce_by_frame(7);
        release(&id);
        assert_eq!(realised(&id), None);
        assert_eq!(
            realised_in_room(PHOTOS, RoomId::parse(ROOM).unwrap()),
            None,
            "and the coarse answer goes with it"
        );
        assert!(
            !announced_by_frame(7),
            "the tab is gone, so the projection has nothing to hold back"
        );
        assert!(claim(id, 8), "and the next ask opens a window");
    }

    /// An open naming no application is not an open. Two of them must not
    /// collapse into one, or the first unnamed request would swallow every
    /// later one.
    #[test]
    #[serial_test::serial(process_env)]
    fn an_unnamed_open_claims_nothing() {
        clear_for_test();
        let nothing = OpenIdentity::new("   ", ROOM, None);
        assert!(!nothing.is_named());
        assert!(claim(nothing.clone(), 1));
        assert!(claim(nothing.clone(), 2), "and does not stop the next one");
        assert_eq!(realised(&nothing), None);
    }

    /// A room id that is not one — the unminted nil sentinel, or a string
    /// that is not a uuid — names no room.
    #[test]
    #[serial_test::serial(process_env)]
    fn an_unminted_room_is_no_room() {
        clear_for_test();
        let nil = OpenIdentity::new(PHOTOS, "00000000-0000-0000-0000-000000000000", None);
        let empty = OpenIdentity::new(PHOTOS, "", None);
        assert_eq!(
            nil, empty,
            "a workspace with no room identity yet is a workspace with no room"
        );
        assert_eq!(nil, OpenIdentity::in_room(PHOTOS, RoomId::nil(), None));
    }

    /// The two ways of naming a room have to produce one identity, or the
    /// mechanism reading a wire field and the one reading a workspace would
    /// never recognise each other's opens.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_cluster_id_and_a_workspace_name_the_same_room() {
        clear_for_test();
        let room = RoomId::parse(ROOM).expect("the fixture is a uuid");
        assert_eq!(
            OpenIdentity::new(PHOTOS, ROOM, None),
            OpenIdentity::in_room(PHOTOS, room, None),
            "the cluster id an announcement carries is the workspace's own room"
        );
        assert_eq!(
            OpenIdentity::new(PHOTOS, &ROOM.to_ascii_uppercase(), None),
            OpenIdentity::in_room(PHOTOS, room, None),
            "and it is the same room however the uuid was spelled"
        );
    }
}

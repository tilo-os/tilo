//! You, as the people in each workspace see you: how available you say you
//! are (and what your machine knows: idle, locked), what they see of you
//! in each workspace, and your waves.
//!
//! What people see is chosen per workspace ([`ui::PresenceVisibility`]):
//! a public workspace starts at "That I'm here", a private one at "Where I
//! am". Until the host says whether a workspace with a host is public, it
//! is treated as public: nothing of where you are leaves before that is
//! known. The choice is handed to the network
//! ([`super::ShellOverlayHost::set_presence_visibility_bridge`]), which is
//! where every pointer, caret and live frame is held back.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use crate::shell::chrome_state::PublicationState;
use crate::shell::state::RoomId;

/// Publishes the desktop's own presence (availability, arriving, leaving)
/// in a room.
pub(crate) type DesktopPresenceBridge =
    std::rc::Rc<dyn Fn(RoomId, ui::collaboration::PresencePayload)>;
/// Waves at a person here (a roster id) on the room's live presence, as a
/// browser tab waves; returns whether it went out.
pub(crate) type WaveBridge = std::rc::Rc<dyn Fn(RoomId, &str) -> bool>;
/// Tells the network what the people in a room may see of this person.
pub(crate) type VisibilityBridge = std::rc::Rc<dyn Fn(RoomId, ui::PresenceVisibility)>;

/// One wave at the same person a minute, as in a browser tab.
pub(crate) const WAVE_EVERY: Duration = Duration::from_millis(tilo_desktop::here::WAVE_AGAIN_MS);

#[derive(Default)]
pub(crate) struct SelfPresence {
    /// What the person chose: available, focusing or away.
    pub(crate) chosen: ui::Availability,
    /// What they chose people see, per workspace room; absent rooms take
    /// their default.
    pub(crate) visibility: BTreeMap<RoomId, ui::PresenceVisibility>,
    /// What each room's people may see, as last worked out (and told to
    /// the network).
    pub(crate) pushed: HashMap<RoomId, ui::PresenceVisibility>,
    /// What every room's visibility was last worked out from: its room,
    /// what its host said, and the person's choice. Nothing is worked out
    /// again until one of them changes.
    pub(crate) visibility_inputs: Vec<(RoomId, u8, Option<ui::PresenceVisibility>)>,
    /// What was last published, and where: to the workspace on screen,
    /// when its people may see where this person is.
    pub(crate) published: Option<(RoomId, ui::Availability, Option<String>)>,
    /// What was last published to each workspace whose people may see only
    /// that this person is here: what they chose, told whether or not the
    /// workspace is on screen.
    pub(crate) published_here: HashMap<RoomId, ui::Availability>,
    pub(crate) locked: bool,
    pub(crate) quiet_for: Duration,
    /// The voice channel this person is in, as the call says.
    pub(crate) call: Option<String>,
    /// Per room where this person shows nothing, the people there when its
    /// people were last told this person is not here: told again whenever
    /// someone new is there, who has not heard it.
    pub(crate) told_absent: HashMap<RoomId, std::collections::BTreeSet<String>>,
    /// When this person last waved at whom.
    pub(crate) waved: HashMap<String, Instant>,
    pub(crate) desktop_bridge: Option<DesktopPresenceBridge>,
    pub(crate) wave_bridge: Option<WaveBridge>,
    pub(crate) visibility_bridge: Option<VisibilityBridge>,
}

impl SelfPresence {
    /// What the person shows as now.
    pub(crate) fn shown(&self) -> ui::Availability {
        ui::Availability::shown(self.chosen, self.locked, self.quiet_for)
    }

    /// What the people of a workspace where they see this person as
    /// `visibility` are told of how available they are: under "Where I am"
    /// also what the machine knows (a locked screen is away, a quiet one
    /// idle); under "That I'm here" only what the person chose, never what
    /// is inferred from what they do.
    pub(crate) fn shown_to(&self, visibility: ui::PresenceVisibility) -> ui::Availability {
        if visibility.shows_where() {
            self.shown()
        } else {
            self.chosen
        }
    }

    /// Wave at `target` now, unless they were waved at in the last minute.
    pub(crate) fn may_wave(&self, target: &str, now: Instant) -> bool {
        self.waved
            .get(target)
            .is_none_or(|at| now.saturating_duration_since(*at) >= WAVE_EVERY)
    }

    /// Whom this person waved at within the minute: their rows show it sent.
    pub(crate) fn waved_recently(&self, now: Instant) -> Vec<String> {
        self.waved
            .iter()
            .filter(|(_, at)| now.saturating_duration_since(**at) < WAVE_EVERY)
            .map(|(target, _)| target.clone())
            .collect()
    }

    /// Record a wave at `target` sent at `now`.
    pub(crate) fn note_waved(&mut self, target: &str, now: Instant) {
        self.waved.insert(target.to_owned(), now);
    }
}

/// What the people in a workspace see of this person: their choice, else
/// the workspace's default. `site` is the room as it is held through the
/// web, if it is: a site's home workspace (a community such as
/// tilo.coffee's) and one joined from a directory are public. Otherwise
/// `publication` is what its host said about it, `None` before anyone
/// asked: a workspace not known yet is held to the public default, so
/// nothing of where you are leaves before that is known.
pub(crate) fn visibility_of(
    chosen: Option<ui::PresenceVisibility>,
    site: Option<&crate::shell::network::SiteRoom>,
    publication: Option<&PublicationState>,
) -> ui::PresenceVisibility {
    if let Some(chosen) = chosen {
        return chosen;
    }
    let public = match (site, publication) {
        (Some(site), _) => site.community || site.public,
        (None, Some(PublicationState::Known(publication))) => publication.public,
        // No host lists it, or someone else runs it privately.
        (None, Some(PublicationState::Unavailable(_))) => false,
        (None, Some(PublicationState::Asking) | None) => true,
    };
    ui::PresenceVisibility::default_for(public)
}

impl super::ShellOverlayHost {
    /// Install where waves go out.
    pub fn set_wave_bridge(&mut self, bridge: WaveBridge) {
        self.self_presence.wave_bridge = Some(bridge);
    }

    /// Install the publisher of the desktop's own presence.
    pub fn set_desktop_presence_bridge(&mut self, bridge: DesktopPresenceBridge) {
        self.self_presence.desktop_bridge = Some(bridge);
        self.self_presence.published = None;
    }

    /// Install where what people see of this person is enforced.
    pub fn set_presence_visibility_bridge(&mut self, bridge: VisibilityBridge) {
        for (room, visibility) in &self.self_presence.pushed {
            bridge(*room, *visibility);
        }
        self.self_presence.visibility_bridge = Some(bridge);
    }

    /// What the machine knows: the screen is locked, or nothing was typed
    /// or moved for `quiet_for`.
    pub(crate) fn note_local_activity(&mut self, locked: bool, quiet_for: Duration) {
        self.self_presence.locked = locked;
        self.self_presence.quiet_for = quiet_for;
    }

    /// What the people in the workspace of `room` see of this person.
    pub(crate) fn presence_visibility(&self, room: RoomId) -> ui::PresenceVisibility {
        let Some(chrome) = self.chrome.as_ref() else {
            return ui::PresenceVisibility::default_for(false);
        };
        let site = self.site_rooms().into_iter().find(|site| site.room == room);
        let state = chrome.borrow();
        visibility_of(
            self.self_presence.visibility.get(&room).copied(),
            site.as_ref(),
            state.publication(room),
        )
    }

    /// Whether the active workspace is public, as its host last said.
    pub(crate) fn active_workspace_is_public(&self) -> bool {
        self.active_room()
            .is_some_and(|room| self.room_is_public(room))
    }

    /// Whether the workspace of `room` is public: a site's home or one
    /// joined from a directory, or one its host lists.
    pub(crate) fn room_is_public(&self, room: RoomId) -> bool {
        #[cfg(feature = "hosted-test-control")]
        if self.public_for_shots {
            return true;
        }
        if let Some(site) = self.site_rooms().into_iter().find(|site| site.room == room) {
            return site.community || site.public;
        }
        self.chrome.as_ref().is_some_and(|chrome| {
            matches!(
                chrome.borrow().publication(room),
                Some(PublicationState::Known(publication)) if publication.public
            )
        })
    }

    /// Choose what the people in the workspace of `room` see.
    pub(crate) fn set_presence_visibility(&mut self, room: RoomId, choice: ui::PresenceVisibility) {
        self.self_presence.visibility.insert(room, choice);
        self.persist_collaboration_preferences();
        self.sync_self_presence();
    }

    /// Choose how available this person says they are.
    pub(crate) fn set_chosen_availability(&mut self, chosen: ui::Availability) {
        // Idle is the machine's to say, never a choice.
        let chosen = if chosen == ui::Availability::Idle {
            ui::Availability::Available
        } else {
            chosen
        };
        self.self_presence.chosen = chosen;
        self.persist_collaboration_preferences();
        self.sync_self_presence();
    }

    /// Wave at the collaborator `target` (a roster id) in the active
    /// workspace: their bar says who waved and bounces their face, and
    /// their row here shows it sent. Once a minute per person; returns
    /// whether it went.
    pub(crate) fn wave_at(&mut self, target: &str) -> bool {
        let now = Instant::now();
        let Some(room) = self.active_room() else {
            return false;
        };
        if !self.self_presence.may_wave(target, now)
            || !self.presence_visibility(room).shows_presence()
        {
            return false;
        }
        let Some(bridge) = self.self_presence.wave_bridge.clone() else {
            return false;
        };
        if !bridge(room, target) {
            return false;
        }
        self.self_presence.note_waved(target, now);
        true
    }

    /// Waves that reached this person (who waved, by roster id): the top
    /// bar says the last one, named as the roster names them, as a browser
    /// tab's bar does. Returns whether there was one.
    pub(crate) fn receive_waves(&self, from: &[String]) -> bool {
        let (Some(from), Some(chrome)) = (from.last(), self.chrome.clone()) else {
            return false;
        };
        let mut state = chrome.borrow_mut();
        let local = state.active_local_user();
        let name =
            tilo_desktop::collaborators::collaborator_roster_rows(&state.tree().desktop, &local)
                .into_iter()
                .find(|row| row.id == *from)
                .map_or_else(
                    || crate::shell::i18n::t("presence-someone"),
                    |row| row.label,
                );
        state.wave_notice = Some(tilo_desktop::chrome::WaveNotice {
            from: name,
            at: ui::time::now(),
        });
        state.bump();
        true
    }

    pub(super) fn active_room(&self) -> Option<RoomId> {
        let chrome = self.chrome.as_ref()?;
        let state = chrome.borrow();
        state
            .tree()
            .desktop
            .active_workspace()
            .map(crate::shell::state::Workspace::room)
    }

    /// Tell the network what every workspace's people may see, and the
    /// active workspace's people how available this person is, each when it
    /// changed. Asks a hosted workspace's host whether it is public the
    /// first time it matters. Returns whether anything went out.
    pub(crate) fn sync_self_presence(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let rooms: Vec<RoomId> = chrome
            .borrow()
            .tree()
            .desktop
            .workspaces
            .iter()
            .map(crate::shell::state::Workspace::room)
            .collect();
        let inputs: Vec<(RoomId, u8, Option<ui::PresenceVisibility>)> = {
            let state = chrome.borrow();
            rooms
                .iter()
                .map(|room| {
                    let publication = match state.publication(*room) {
                        None => 0,
                        Some(PublicationState::Asking) => 1,
                        Some(PublicationState::Unavailable(_)) => 2,
                        Some(PublicationState::Known(known)) => {
                            if known.public {
                                4
                            } else {
                                3
                            }
                        }
                    };
                    (
                        *room,
                        publication,
                        self.self_presence.visibility.get(room).copied(),
                    )
                })
                .collect()
        };
        let mut sent = false;
        if inputs != self.self_presence.visibility_inputs {
            sent = self.push_visibilities(&rooms);
            self.self_presence.visibility_inputs = inputs;
        }
        // The voice channel this device is in is this person's call, as its
        // people see it; leaving the call clears it.
        self.self_presence.call = self.own_voice_call().map(|call| call.encode());
        sent | self.tell_absence() | self.publish_availability()
    }

    /// In every room where this person shows nothing, tell its people they
    /// are not here (a room-keyed `Left`, which "Nothing" still lets out),
    /// so nobody lists them even though their machine keeps taking part in
    /// the room's documents. Told once when the choice takes hold, and again
    /// whenever someone is there who has not heard it. Returns whether
    /// anything went out.
    fn tell_absence(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let hidden: Vec<RoomId> = self
            .self_presence
            .pushed
            .iter()
            .filter(|(_, visibility)| !visibility.shows_presence())
            .map(|(room, _)| *room)
            .collect();
        self.self_presence
            .told_absent
            .retain(|room, _| hidden.contains(room));
        let Some(bridge) = self.self_presence.desktop_bridge.clone() else {
            return false;
        };
        let mut sent = false;
        for room in hidden {
            let here: std::collections::BTreeSet<String> = {
                let state = chrome.borrow();
                let tree = state.tree();
                tree.desktop
                    .workspaces
                    .iter()
                    .filter(|workspace| workspace.room() == room)
                    .flat_map(|workspace| &workspace.collaborators)
                    .filter(|person| !person.invited)
                    .map(|person| person.id.clone())
                    .collect()
            };
            let told = self.self_presence.told_absent.get(&room);
            if told.is_some_and(|told| here.is_subset(told)) {
                continue;
            }
            bridge(room, ui::collaboration::PresencePayload::Left);
            self.self_presence.told_absent.insert(room, here);
            sent = true;
        }
        sent
    }

    /// Work out what every room's people may see and tell the network the
    /// ones that changed; ask the host of each room not known yet whether
    /// it is public.
    fn push_visibilities(&mut self, rooms: &[RoomId]) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let sites = self.site_rooms();
        let mut sent = false;
        for room in rooms {
            if !self.self_presence.visibility.contains_key(room)
                && !sites.iter().any(|site| site.room == *room)
                && chrome.borrow().publication(*room).is_none()
            {
                if self.peer_sync.is_running() {
                    chrome.borrow_mut().ask_publication(*room);
                } else {
                    // Nobody to ask without peer-sync: the answer an ask
                    // would get, without a request whose answer redraws the
                    // desktop after its first quiet frame.
                    chrome
                        .borrow_mut()
                        .set_publication(*room, Err(super::peer_sync_dispatch::NOT_RUNNING.into()));
                }
            }
            let visibility = {
                let state = chrome.borrow();
                visibility_of(
                    self.self_presence.visibility.get(room).copied(),
                    sites.iter().find(|site| site.room == *room),
                    state.publication(*room),
                )
            };
            let was = self.self_presence.pushed.insert(*room, visibility);
            if was != Some(visibility) {
                // Seen again, or no longer: availability is said afresh
                // once the room's people may see this person, and they are
                // told anew that this person is not here once they may not.
                if self
                    .self_presence
                    .published
                    .as_ref()
                    .is_some_and(|(published, _, _)| published == room)
                {
                    self.self_presence.published = None;
                }
                self.self_presence.told_absent.remove(room);
                if let Some(bridge) = self.self_presence.visibility_bridge.clone() {
                    bridge(*room, visibility);
                    sent = true;
                }
                // Back from showing nothing: say so in that room at once,
                // whether or not it is on screen, so its people list this
                // person again (they were told this person is not here).
                if was.is_some_and(|was| !was.shows_presence())
                    && visibility.shows_presence()
                    && let Some(bridge) = self.self_presence.desktop_bridge.clone()
                {
                    let state = self.self_presence.shown_to(visibility);
                    bridge(
                        *room,
                        ui::collaboration::PresencePayload::Availability {
                            state,
                            call: self
                                .self_presence
                                .call
                                .clone()
                                .filter(|_| visibility.shows_where()),
                        },
                    );
                    if !visibility.shows_where() {
                        self.self_presence.published_here.insert(*room, state);
                    }
                    sent = true;
                }
            }
        }
        self.self_presence
            .pushed
            .retain(|room, _| rooms.contains(room));
        sent
    }

    /// Say how available this person is, each time that changed: in the
    /// workspace on screen when its people may see where this person is
    /// (with what the machine knows, and the call they are in); and in
    /// every workspace whose people may see only that they are here, what
    /// they chose, whether or not it is on screen: telling it only once it
    /// is on screen would say which workspace they are looking at.
    fn publish_availability(&mut self) -> bool {
        let Some(bridge) = self.self_presence.desktop_bridge.clone() else {
            return false;
        };
        let mut sent = false;
        if let Some(room) = self.active_room()
            && self
                .self_presence
                .pushed
                .get(&room)
                .is_some_and(|visibility| visibility.shows_where())
        {
            let shown = self.self_presence.shown();
            let call = self.self_presence.call.clone();
            let now = (room, shown, call.clone());
            if self.self_presence.published.as_ref() != Some(&now) {
                bridge(
                    room,
                    ui::collaboration::PresencePayload::Availability { state: shown, call },
                );
                self.self_presence.published = Some(now);
                sent = true;
            }
        }
        let here: Vec<RoomId> = self
            .self_presence
            .pushed
            .iter()
            .filter(|(_, visibility)| visibility.shows_presence() && !visibility.shows_where())
            .map(|(room, _)| *room)
            .collect();
        self.self_presence
            .published_here
            .retain(|room, _| here.contains(room));
        let chosen = self.self_presence.chosen;
        for room in here {
            if self.self_presence.published_here.get(&room) == Some(&chosen) {
                continue;
            }
            bridge(
                room,
                ui::collaboration::PresencePayload::Availability {
                    state: chosen,
                    call: None,
                },
            );
            self.self_presence.published_here.insert(room, chosen);
            sent = true;
        }
        sent
    }
}

#[cfg(feature = "hosted-test-control")]
impl super::ShellOverlayHost {
    /// Follow `id` as the roster names them, for captures.
    pub fn follow_for_shots(&mut self, id: Option<&str>) {
        self.set_followed_peer(id.map(str::to_owned));
    }

    /// Open the roster row of `id`, for captures.
    pub fn expand_collaborator_for_shots(&mut self, id: &str) {
        self.expanded_collaborator = Some(id.to_owned());
    }

    /// The person's own input ended following, for captures.
    pub fn stop_following_by_input_for_shots(&mut self) {
        self.stop_following_for_local_input();
    }

    /// `from` waved at this person, for captures.
    pub fn wave_from_for_shots(&mut self, from: &str) {
        self.receive_waves(&[from.to_owned()]);
    }

    /// Show the arrival line for the workspace on screen as if just arrived,
    /// for captures; `public` tells the person what its people see.
    pub fn arrive_for_shots(&mut self, public: bool) {
        self.presence_notices = super::arrival::PresenceNotices::default();
        if !public && let Some(room) = self.active_room() {
            self.presence_notices.introduced.insert(room);
        }
        self.public_for_shots = public;
        self.refresh_presence_notices();
    }

    /// Open cursor chat with `text` typed, for captures.
    pub fn open_cursor_chat_for_shots(&mut self, text: &str) {
        self.cursor_chat_open_for_shots(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(public: bool) -> PublicationState {
        let room = RoomId::new();
        PublicationState::Known(crate::shell::network::Publication {
            public,
            ..crate::shell::network::Publication::private(room)
        })
    }

    /// A public workspace starts at "That I'm here", a private one at
    /// "Where I am"; one not known yet is held to the public default; a
    /// choice always wins. tilo.coffee's Lounge is a community site's home.
    #[test]
    fn a_public_workspace_shows_that_you_are_here_and_a_private_one_where_you_are() {
        use ui::PresenceVisibility::{Nothing, ThatImHere, WhereIAm};
        let lounge = crate::shell::network::SiteRoom {
            room: RoomId::new(),
            site: Some("tilo.coffee".into()),
            community: true,
            public: false,
        };
        let listed = crate::shell::network::SiteRoom {
            community: false,
            public: true,
            ..lounge.clone()
        };
        let invited = crate::shell::network::SiteRoom {
            site: None,
            community: false,
            public: false,
            ..lounge
        };
        assert_eq!(visibility_of(None, Some(&lounge), None), ThatImHere);
        assert_eq!(visibility_of(None, Some(&listed), None), ThatImHere);
        assert_eq!(visibility_of(None, Some(&invited), None), WhereIAm);
        assert_eq!(visibility_of(None, None, Some(&known(true))), ThatImHere);
        assert_eq!(visibility_of(None, None, Some(&known(false))), WhereIAm);
        assert_eq!(
            visibility_of(None, None, None),
            ThatImHere,
            "a workspace not known yet is held to the public default"
        );
        assert_eq!(
            visibility_of(None, None, Some(&PublicationState::Asking)),
            ThatImHere
        );
        assert_eq!(
            visibility_of(
                None,
                None,
                Some(&PublicationState::Unavailable("no host".into()))
            ),
            WhereIAm
        );
        assert_eq!(
            visibility_of(Some(Nothing), None, Some(&known(false))),
            Nothing
        );
        assert_eq!(visibility_of(Some(WhereIAm), Some(&lounge), None), WhereIAm);
    }

    /// "Nothing" means not appearing: choosing it tells the room this
    /// person is not here, once, and again for anyone new there who has not
    /// heard it; showing themselves again says they are, at once.
    #[test]
    fn showing_nothing_tells_the_room_you_are_not_here_and_coming_back_says_you_are() {
        use ui::collaboration::PresencePayload;
        let tree = crate::shell::state::WorkspaceTree::bootstrap_demo();
        let room = tree
            .desktop
            .active_workspace()
            .expect("the demo has a workspace on screen")
            .room();
        let (mut host, chrome) = crate::shell::build_tilo_overlay_host_with_chrome(&tree);
        let said = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = std::rc::Rc::clone(&said);
        host.set_desktop_presence_bridge(std::rc::Rc::new(move |room, payload| {
            sink.borrow_mut().push((room, payload));
        }));
        let lefts = |said: &[(RoomId, PresencePayload)]| {
            said.iter()
                .filter(|(at, payload)| *at == room && matches!(payload, PresencePayload::Left))
                .count()
        };

        host.self_presence
            .visibility
            .insert(room, ui::PresenceVisibility::Nothing);
        host.sync_self_presence();
        assert_eq!(
            lefts(&said.borrow()),
            1,
            "the room is told this person is not here"
        );
        assert!(
            !said
                .borrow()
                .iter()
                .any(|(_, payload)| matches!(payload, PresencePayload::Availability { .. })),
            "and nothing else of them"
        );
        host.sync_self_presence();
        assert_eq!(lefts(&said.borrow()), 1, "once");

        {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            tree.desktop
                .active_workspace_mut()
                .expect("the workspace on screen")
                .collaborators
                .push(crate::shell::state::Collaborator::new("newcomer", "Ada"));
        }
        chrome.borrow_mut().bump();
        host.sync_self_presence();
        assert_eq!(
            lefts(&said.borrow()),
            2,
            "and again once someone is there who has not heard it"
        );

        host.self_presence
            .visibility
            .insert(room, ui::PresenceVisibility::WhereIAm);
        host.sync_self_presence();
        assert!(
            said.borrow().iter().any(|(at, payload)| *at == room
                && matches!(payload, PresencePayload::Availability { .. })),
            "showing themselves again says they are here"
        );
    }

    /// Under "That I'm here" the people of a workspace learn how available
    /// this person chose to be, and nothing the machine infers: a locked or
    /// quiet screen says nothing, the call they are in says nothing, and
    /// whether the workspace is on screen says nothing either. Under "Where
    /// I am" the workspace on screen hears all of it.
    #[test]
    fn that_im_here_tells_only_the_chosen_availability_wherever_you_look() {
        use ui::collaboration::PresencePayload;
        let tree = crate::shell::state::WorkspaceTree::bootstrap_demo();
        let room = tree
            .desktop
            .active_workspace()
            .expect("the demo has a workspace on screen")
            .room();
        let (mut host, _chrome) = crate::shell::build_tilo_overlay_host_with_chrome(&tree);
        let said = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = std::rc::Rc::clone(&said);
        host.set_desktop_presence_bridge(std::rc::Rc::new(move |room, payload| {
            sink.borrow_mut().push((room, payload));
        }));
        let availability = |said: &[(RoomId, PresencePayload)]| {
            said.iter()
                .filter_map(|(at, payload)| match payload {
                    PresencePayload::Availability { state, call } if *at == room => {
                        Some((*state, call.clone()))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        host.self_presence
            .visibility
            .insert(room, ui::PresenceVisibility::ThatImHere);
        host.note_local_activity(true, Duration::from_secs(3600));
        host.sync_self_presence();
        assert_eq!(
            availability(&said.borrow()),
            [(ui::Availability::Available, None)],
            "what they chose, not the locked screen"
        );
        host.sync_self_presence();
        assert_eq!(availability(&said.borrow()).len(), 1, "said once");
        host.note_local_activity(false, Duration::ZERO);
        host.sync_self_presence();
        assert_eq!(
            availability(&said.borrow()).len(),
            1,
            "coming back to the screen says nothing"
        );

        host.self_presence
            .visibility
            .insert(room, ui::PresenceVisibility::WhereIAm);
        host.note_local_activity(true, Duration::ZERO);
        host.sync_self_presence();
        assert!(
            availability(&said.borrow())
                .iter()
                .any(|(state, _)| *state == ui::Availability::Away),
            "\"Where I am\" tells the locked screen"
        );
    }

    #[test]
    fn waves_are_once_a_minute_per_person() {
        let mut me = SelfPresence::default();
        let now = Instant::now();
        assert!(me.may_wave("peer@1", now));
        me.note_waved("peer@1", now);
        assert!(!me.may_wave("peer@1", now + Duration::from_secs(30)));
        assert!(me.may_wave("peer@2", now));
        assert!(me.may_wave("peer@1", now + WAVE_EVERY));
    }
}

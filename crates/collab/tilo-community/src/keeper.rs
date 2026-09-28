//! Keeping a person where they want to be in the Tilo Community, for any
//! host that holds their rooms.
//!
//! [`CommunityKeeper`] runs the rules ([`AutoJoin`]) against what the host
//! observes and has the host perform each step through [`CommunityHost`]:
//! the native shell over its workspace tree and network actor, a hosted
//! desktop over its own rooms and relay sessions. The rules, the order of
//! the steps and what a refusal means are this crate's, so every host keeps
//! a person in the community the same way.

use std::time::{Duration, Instant};

use iroh_base::EndpointId;
use settings_store::community::Membership;
use tilo_desktop_model::RoomId;

use crate::config::CommunityConfig;
use crate::join::{AutoJoin, Observed, Step};

/// How often a host looks at whether the person is where they want to be.
/// Joins themselves back off on their own schedule ([`AutoJoin`]).
pub const COMMUNITY_CHECK_EVERY: Duration = Duration::from_secs(1);

/// The configured community: its standing invitation and what it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StandingCommunity {
    /// The invitation's bearer string.
    pub invite: String,
    /// The invitation's id, as hex: what a refusal is remembered by.
    pub invite_hex: String,
    /// The community room.
    pub room: RoomId,
    /// The room's hosts, the endpoints a refusal of it can come from.
    pub hosts: Vec<EndpointId>,
}

/// The community `config` names.
///
/// # Errors
///
/// The community is not configured, or its invitation does not validate, is
/// not a standing one or names no room.
pub fn standing(config: &CommunityConfig) -> Result<StandingCommunity, String> {
    let invite = config.invite.clone().ok_or_else(|| {
        format!(
            "no community invitation is configured in {}",
            config.source.display()
        )
    })?;
    let parsed = neighbour_wire::invite::validate_invite_token(&invite)?;
    if !parsed.standing {
        return Err("the configured community invitation is not a standing invitation".into());
    }
    let room = RoomId::parse(&parsed.room_id)
        .ok_or_else(|| "the community invitation names no room".to_string())?;
    Ok(StandingCommunity {
        invite_hex: hex::encode(neighbour_wire::invite::invite_id(&invite)),
        hosts: parsed.host_addrs.iter().map(|host| host.id).collect(),
        invite,
        room,
    })
}

/// How a host holds a room.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Holding {
    /// Not held.
    #[default]
    No,
    /// Held, but its host has not admitted this device yet.
    Held,
    /// Held with a role: its host admitted this device.
    Admitted,
}

/// What a host does for the community, and what it tells about itself.
pub trait CommunityHost {
    /// The host has a certified identity to join with.
    fn has_identity(&self) -> bool;
    /// The person's choice, as their settings say.
    fn membership(&self) -> Membership;
    /// The invitation the community's host refused, as remembered.
    fn refused_invite(&self) -> Option<String>;
    /// How this host holds `room`.
    fn holding(&self, room: RoomId) -> Holding;
    /// Hold the community room and redeem its invitation.
    ///
    /// # Errors
    ///
    /// When joining does not start; it is tried again later.
    fn join(&mut self, community: &StandingCommunity) -> Result<(), String>;
    /// Ask the room's hosts again to admit this device.
    fn redeem(&mut self, community: &StandingCommunity);
    /// Open the private room with the Tilo team.
    ///
    /// # Errors
    ///
    /// When there is none: no team configured, or this is the team.
    fn open_team_room(&mut self) -> Result<(), String>;
    /// Remember that the community's host refused `invite_hex`.
    ///
    /// # Errors
    ///
    /// When it cannot be remembered.
    fn remember_refusal(&mut self, invite_hex: &str) -> Result<(), String>;
    /// Leave `room`.
    ///
    /// # Errors
    ///
    /// When it cannot be left.
    fn leave(&mut self, room: RoomId) -> Result<(), String>;
}

/// The redemption in flight, so a refusal by the community's own host is
/// told from any other host's.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Redeeming {
    room: RoomId,
    hosts: Vec<EndpointId>,
    invite_hex: String,
}

/// What went wrong while keeping the community, for the host's log. Each is
/// tried again later on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeeperNote {
    JoinDidNotStart(String),
    NoTeamRoom(String),
    RefusalNotRemembered(String),
    CouldNotLeave(String),
}

/// Keeps a person where they want to be in the Tilo Community. Everyone is
/// a member by default: with an identity and a configured community, the
/// host joins once it can, asks the community's hosts again with back-off
/// while they cannot be reached, and opens the private room with the Tilo
/// team once it is in. A person who left is never joined again, and a host
/// that learns it leaves the room. A refusal by the community's host is
/// remembered for that invitation and not asked again.
#[derive(Clone, Debug, Default)]
pub struct CommunityKeeper {
    auto: AutoJoin,
    checked: Option<Instant>,
    redeeming: Option<Redeeming>,
}

impl CommunityKeeper {
    /// Whether [`Self::tick`] at `now` looks at the community, so a host can
    /// skip gathering what it observes in between.
    pub fn is_due(&self, now: Instant) -> bool {
        self.checked
            .is_none_or(|checked| now.saturating_duration_since(checked) >= COMMUNITY_CHECK_EVERY)
    }

    /// Look at the community at most every [`COMMUNITY_CHECK_EVERY`] and
    /// perform what is due through `host`. `community` is the configured
    /// one, or why there is none.
    pub fn tick(
        &mut self,
        now: Instant,
        community: Result<StandingCommunity, String>,
        host: &mut dyn CommunityHost,
    ) -> Vec<KeeperNote> {
        let mut notes = Vec::new();
        if !self.is_due(now) {
            return notes;
        }
        self.checked = Some(now);
        let Ok(community) = community else {
            self.auto.next(now, &Observed::default());
            return notes;
        };
        let holding = host.holding(community.room);
        let observed = Observed {
            identity: host.has_identity(),
            invite: Some(community.invite_hex.clone()),
            membership: host.membership(),
            refused_invite: host.refused_invite(),
            held: holding != Holding::No,
            admitted: holding == Holding::Admitted,
        };
        let step = self.auto.next(now, &observed);
        match step {
            Step::Wait => {}
            Step::Join | Step::Redeem => {
                self.redeeming = Some(Redeeming {
                    room: community.room,
                    hosts: community.hosts.clone(),
                    invite_hex: community.invite_hex.clone(),
                });
                self.auto.attempted(now);
                if step == Step::Join {
                    if let Err(error) = host.join(&community) {
                        notes.push(KeeperNote::JoinDidNotStart(error));
                    }
                } else {
                    host.redeem(&community);
                }
            }
            Step::OpenTeamRoom => {
                self.auto.team_room_opened();
                self.redeeming = None;
                if let Err(error) = host.open_team_room() {
                    notes.push(KeeperNote::NoTeamRoom(error));
                }
            }
            Step::RecordRefusal | Step::Leave => {
                self.redeeming = None;
                if step == Step::RecordRefusal
                    && let Err(error) = host.remember_refusal(&community.invite_hex)
                {
                    notes.push(KeeperNote::RefusalNotRemembered(error));
                }
                if observed.held
                    && let Err(error) = host.leave(community.room)
                {
                    notes.push(KeeperNote::CouldNotLeave(error));
                }
            }
        }
        notes
    }

    /// A host refused this device for good on a room `is_room` recognises,
    /// from endpoint `peer`. When it is the community's host refusing the
    /// invitation being redeemed, the refusal is final for that invitation.
    /// Returns whether it was.
    pub fn note_refusal(
        &mut self,
        is_room: impl FnOnce(RoomId) -> bool,
        peer: &EndpointId,
    ) -> bool {
        let Some(redeeming) = self.redeeming.clone() else {
            return false;
        };
        if is_room(redeeming.room) && redeeming.hosts.contains(peer) {
            self.auto.refused(&redeeming.invite_hex);
            self.checked = None;
            return true;
        }
        false
    }

    /// The person chose to join again: whatever was pending starts over and
    /// the community is looked at on the next tick.
    pub fn rejoin(&mut self) {
        self.auto.reset();
        self.checked = None;
    }

    /// The person chose to leave: nothing pending is followed up.
    pub fn left(&mut self) {
        self.rejoin();
        self.redeeming = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOM: &str = "7a1a9e00-0000-4c0d-9a11-0000000000c1";

    #[derive(Default)]
    struct Host {
        membership: Membership,
        refused: Option<String>,
        holding: Holding,
        done: Vec<String>,
    }

    impl CommunityHost for Host {
        fn has_identity(&self) -> bool {
            true
        }
        fn membership(&self) -> Membership {
            self.membership
        }
        fn refused_invite(&self) -> Option<String> {
            self.refused.clone()
        }
        fn holding(&self, _room: RoomId) -> Holding {
            self.holding
        }
        fn join(&mut self, _community: &StandingCommunity) -> Result<(), String> {
            self.done.push("join".into());
            self.holding = Holding::Held;
            Ok(())
        }
        fn redeem(&mut self, _community: &StandingCommunity) {
            self.done.push("redeem".into());
        }
        fn open_team_room(&mut self) -> Result<(), String> {
            self.done.push("team".into());
            Ok(())
        }
        fn remember_refusal(&mut self, invite_hex: &str) -> Result<(), String> {
            self.done.push(format!("refused {invite_hex}"));
            self.refused = Some(invite_hex.into());
            Ok(())
        }
        fn leave(&mut self, _room: RoomId) -> Result<(), String> {
            self.done.push("leave".into());
            self.holding = Holding::No;
            Ok(())
        }
    }

    fn community(host: EndpointId) -> StandingCommunity {
        StandingCommunity {
            invite: "token".into(),
            invite_hex: "aa".into(),
            room: RoomId::parse(ROOM).unwrap(),
            hosts: vec![host],
        }
    }

    fn endpoint(seed: u8) -> EndpointId {
        iroh_base::SecretKey::from_bytes(&[seed; 32]).public()
    }

    #[test]
    fn a_host_joins_is_admitted_and_opens_the_team_room_once() {
        let start = Instant::now();
        let mut keeper = CommunityKeeper::default();
        let mut host = Host::default();
        let community = || Ok(community(endpoint(1)));
        keeper.tick(start, community(), &mut host);
        assert_eq!(host.done, ["join"]);
        keeper.tick(start, community(), &mut host);
        assert_eq!(host.done, ["join"], "looked at once a second");
        host.holding = Holding::Admitted;
        let later = start + COMMUNITY_CHECK_EVERY;
        keeper.tick(later, community(), &mut host);
        keeper.tick(later + COMMUNITY_CHECK_EVERY, community(), &mut host);
        assert_eq!(host.done, ["join", "team"]);
    }

    #[test]
    fn a_refusal_by_the_communitys_own_host_is_remembered_and_the_room_left() {
        let start = Instant::now();
        let mut keeper = CommunityKeeper::default();
        let mut host = Host::default();
        let community = || Ok(community(endpoint(1)));
        keeper.tick(start, community(), &mut host);
        let room = RoomId::parse(ROOM).unwrap();
        assert!(
            !keeper.note_refusal(|refused| refused == room, &endpoint(2)),
            "another host's refusal is not the community's"
        );
        assert!(keeper.note_refusal(|refused| refused == room, &endpoint(1)));
        keeper.tick(start, community(), &mut host);
        assert_eq!(host.done, ["join", "refused aa", "leave"]);
    }

    #[test]
    fn a_person_who_left_is_taken_out_and_not_joined_again() {
        let start = Instant::now();
        let mut keeper = CommunityKeeper::default();
        let mut host = Host {
            membership: Membership::Left,
            holding: Holding::Admitted,
            ..Host::default()
        };
        keeper.tick(start, Ok(community(endpoint(1))), &mut host);
        keeper.left();
        keeper.tick(start, Ok(community(endpoint(1))), &mut host);
        assert_eq!(host.done, ["leave"]);
    }

    #[test]
    fn an_unconfigured_community_does_nothing() {
        let mut keeper = CommunityKeeper::default();
        let mut host = Host::default();
        keeper.tick(Instant::now(), Err("not configured".into()), &mut host);
        assert!(host.done.is_empty());
    }
}

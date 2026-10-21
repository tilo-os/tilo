//! One seat per agent driving this compositor over the control protocol.
//!
//! Several agents of one person share a hosted machine, and with it the one
//! Wayland seat every app sees. Each agent therefore gets its own strip and
//! its own logical seat: a pointer position, the keys and buttons it holds,
//! and the chrome modifier mask it built. While agents drive the compositor
//! only the focused strip is shown, filling the screen, so on its turn an
//! agent sees its own strip and whatever its actions open over it. Commands arrive
//! one at a time on the compositor thread, so the physical seat is handed from
//! agent to agent between commands: the previous agent's held keys and buttons
//! are released to its window, the next agent's strip takes focus, and what the
//! next agent holds is pressed again. One agent's Shift is never down while
//! another agent types, and a release one agent sends can never end another's
//! hold. Attribution on the shared seat goes through
//! [`RemoteSeats`](crate::compositor::remote_seat::RemoteSeats) under
//! [`AgentSeat::peer`], the same ledger remote collaborators use.

use std::collections::HashMap;

use winit::keyboard::ModifiersState;

use crate::network::protocol::PeerId;
use crate::shell::state::StripId;
use crate::shell::test_control::{KeyTag, PlannedAction, PointerButtonKind};

/// Participant ids handed to agents count down from here, far above the
/// peer ids collaborators receive, so an agent never shares a ledger entry
/// with a person.
const FIRST_AGENT_PEER: PeerId = PeerId::MAX;

/// One agent's logical seat.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentSeat {
    /// Participant id of this agent on the shared seat's ledger.
    pub peer: PeerId,
    /// The strip this agent works in, once one was made or claimed.
    pub strip: Option<StripId>,
    /// Human name the strip was created with.
    pub label: String,
    /// Pointer position relative to the strip's viewport origin.
    cursor: Option<(f64, f64)>,
    /// Keys held, in press order.
    keys: Vec<KeyTag>,
    /// Pointer buttons held, in press order.
    buttons: Vec<PointerButtonKind>,
    /// The chrome modifier mask this agent last left behind.
    modifiers: ModifiersState,
}

impl AgentSeat {
    fn new(peer: PeerId, label: String) -> Self {
        Self {
            peer,
            strip: None,
            label,
            cursor: None,
            keys: Vec::new(),
            buttons: Vec::new(),
            modifiers: ModifiersState::empty(),
        }
    }

    #[must_use]
    pub fn cursor(&self) -> Option<(f64, f64)> {
        self.cursor
    }

    #[must_use]
    pub fn held_keys(&self) -> &[KeyTag] {
        &self.keys
    }

    #[must_use]
    pub fn held_buttons(&self) -> &[PointerButtonKind] {
        &self.buttons
    }

    /// Releases of everything this seat holds, buttons first, latest first.
    fn releases(&self) -> Vec<PlannedAction> {
        let buttons = self
            .buttons
            .iter()
            .rev()
            .map(|button| button_action(*button, false));
        let keys = self
            .keys
            .iter()
            .rev()
            .map(|key| PlannedAction::KeyboardKey {
                key: *key,
                pressed: false,
            });
        buttons.chain(keys).collect()
    }

    /// Restores this seat onto the shared one: pointer back where it was
    /// (in screen space, from `origin`), then its keys, then its buttons.
    fn restores(&self, origin: (f64, f64)) -> Vec<PlannedAction> {
        let cursor = self.cursor.map(|(x, y)| PlannedAction::CursorMoved {
            x: origin.0 + x,
            y: origin.1 + y,
        });
        let keys = self.keys.iter().map(|key| PlannedAction::KeyboardKey {
            key: *key,
            pressed: true,
        });
        let buttons = self
            .buttons
            .iter()
            .map(|button| button_action(*button, true));
        cursor.into_iter().chain(keys).chain(buttons).collect()
    }
}

fn button_action(button: PointerButtonKind, pressed: bool) -> PlannedAction {
    match button {
        PointerButtonKind::Left => PlannedAction::MouseLeftButton { pressed },
        other => PlannedAction::MousePointerButton {
            pressed,
            button: other,
        },
    }
}

/// What handing the shared seat to an agent takes.
#[derive(Debug, Default, PartialEq)]
pub struct Handover {
    /// The agent that held the seat, with the releases its window must see
    /// before focus leaves it. Its holds stay recorded for its next turn.
    pub release: Option<(PeerId, Vec<PlannedAction>)>,
    /// Presses and pointer motion that put the arriving agent's holds back.
    pub restore: Vec<PlannedAction>,
    /// The arriving agent's chrome modifier mask, applied after `restore`.
    pub modifiers: Option<ModifiersState>,
}

impl Handover {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.release.is_none() && self.restore.is_empty() && self.modifiers.is_none()
    }
}

/// Every agent's seat, and which agent holds the shared one now.
#[derive(Debug)]
pub struct AgentSeats {
    seats: HashMap<String, AgentSeat>,
    seated: Option<String>,
    next_peer: PeerId,
}

impl Default for AgentSeats {
    fn default() -> Self {
        Self {
            seats: HashMap::new(),
            seated: None,
            next_peer: FIRST_AGENT_PEER,
        }
    }
}

impl AgentSeats {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn seat(&self, agent: &str) -> Option<&AgentSeat> {
        self.seats.get(agent)
    }

    /// The seat for `agent`, made on first use and labelled `label`.
    pub fn seat_mut(&mut self, agent: &str, label: &str) -> &mut AgentSeat {
        let next_peer = &mut self.next_peer;
        self.seats.entry(agent.to_owned()).or_insert_with(|| {
            let peer = *next_peer;
            *next_peer = next_peer.saturating_sub(1);
            AgentSeat::new(peer, label.to_owned())
        })
    }

    /// The agent holding the shared seat.
    #[must_use]
    pub fn seated(&self) -> Option<&str> {
        self.seated.as_deref()
    }

    /// Whether another agent already works in `strip`.
    #[must_use]
    pub fn strip_owner(&self, strip: StripId) -> Option<&str> {
        self.seats
            .iter()
            .find(|(_, seat)| seat.strip == Some(strip))
            .map(|(agent, _)| agent.as_str())
    }

    /// Hand the shared seat to `agent`, whose strip viewport starts at
    /// `origin`. `live` is the chrome modifier mask right now, which belongs
    /// to whoever holds the seat. Nothing to do when `agent` already has it.
    pub fn hand_over(
        &mut self,
        agent: &str,
        label: &str,
        live: ModifiersState,
        origin: (f64, f64),
    ) -> Handover {
        if self.seated.as_deref() == Some(agent) {
            return Handover::default();
        }
        let release = self
            .seated
            .take()
            .and_then(|previous| self.seats.get_mut(&previous))
            .map(|previous| {
                previous.modifiers = live;
                (previous.peer, previous.releases())
            });
        let seat = self.seat_mut(agent, label);
        let restore = seat.restores(origin);
        let modifiers = Some(seat.modifiers);
        self.seated = Some(agent.to_owned());
        Handover {
            release,
            restore,
            modifiers,
        }
    }

    /// Fold one action the seated `agent` ran into its seat: its pointer
    /// position (relative to `origin`) and what it holds.
    pub fn record(&mut self, agent: &str, action: &PlannedAction, origin: (f64, f64)) {
        let Some(seat) = self.seats.get_mut(agent) else {
            return;
        };
        match action {
            // A pen stroke ends out of range, holding nothing, where it
            // left the cursor.
            PlannedAction::CursorMoved { x, y } | PlannedAction::Pen { x, y, .. } => {
                seat.cursor = Some((x - origin.0, y - origin.1));
            }
            PlannedAction::KeyboardKey { key, pressed } => {
                seat.keys.retain(|held| held != key);
                if *pressed {
                    seat.keys.push(*key);
                }
            }
            PlannedAction::MouseLeftButton { pressed } => {
                hold(&mut seat.buttons, PointerButtonKind::Left, *pressed);
            }
            PlannedAction::MousePointerButton { pressed, button } => {
                hold(&mut seat.buttons, *button, *pressed);
            }
            _ => {}
        }
    }

    /// Forget `agent`. Returns its seat and, when it held the shared seat,
    /// the releases its window must still see.
    pub fn leave(&mut self, agent: &str) -> Option<(AgentSeat, Vec<PlannedAction>)> {
        let seat = self.seats.remove(agent)?;
        let releases = if self.seated.as_deref() == Some(agent) {
            self.seated = None;
            seat.releases()
        } else {
            Vec::new()
        };
        Some((seat, releases))
    }

    /// Number of agents with a seat.
    #[must_use]
    pub fn len(&self) -> usize {
        self.seats.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seats.is_empty()
    }

    /// Diagnostics for `state_dump`: every agent, its strip and holds.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let mut agents = self.seats.iter().collect::<Vec<_>>();
        agents.sort_by(|a, b| a.0.cmp(b.0));
        serde_json::Value::Array(
            agents
                .into_iter()
                .map(|(agent, seat)| {
                    serde_json::json!({
                        "agent": agent,
                        "label": seat.label,
                        "strip": seat.strip.map(|strip| strip.0),
                        "seated": self.seated.as_deref() == Some(agent.as_str()),
                        "held_keys": seat.keys.len(),
                        "held_buttons": seat.buttons.len(),
                    })
                })
                .collect(),
        )
    }
}

fn hold(held: &mut Vec<PointerButtonKind>, button: PointerButtonKind, pressed: bool) {
    held.retain(|candidate| *candidate != button);
    if pressed {
        held.push(button);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::test_control::NamedKeyTag;

    const SHIFT: KeyTag = KeyTag::Named(NamedKeyTag::Shift);

    fn press(key: KeyTag) -> PlannedAction {
        PlannedAction::KeyboardKey { key, pressed: true }
    }

    fn release(key: KeyTag) -> PlannedAction {
        PlannedAction::KeyboardKey {
            key,
            pressed: false,
        }
    }

    #[test]
    fn an_agents_holds_leave_with_it_and_come_back_on_its_next_turn() {
        let mut seats = AgentSeats::new();
        let first = seats.hand_over("a", "A", ModifiersState::empty(), (0.0, 0.0));
        assert_eq!(first.release, None, "nobody held the seat before");
        assert!(first.restore.is_empty());

        seats.record(
            "a",
            &PlannedAction::CursorMoved { x: 40.0, y: 30.0 },
            (0.0, 0.0),
        );
        seats.record("a", &press(SHIFT), (0.0, 0.0));
        seats.record(
            "a",
            &PlannedAction::MouseLeftButton { pressed: true },
            (0.0, 0.0),
        );

        let to_b = seats.hand_over("b", "B", ModifiersState::SHIFT, (0.0, 400.0));
        let peer_a = seats.seat("a").map(|seat| seat.peer);
        assert_eq!(
            to_b.release,
            Some((
                peer_a.unwrap_or_default(),
                vec![
                    PlannedAction::MouseLeftButton { pressed: false },
                    release(SHIFT)
                ]
            )),
            "b's window never sees a's Shift or a's button"
        );
        assert!(to_b.restore.is_empty(), "b holds nothing yet");
        assert_eq!(to_b.modifiers, Some(ModifiersState::empty()));
        assert_eq!(seats.seated(), Some("b"));

        seats.record("b", &press(KeyTag::Char('x')), (0.0, 400.0));
        seats.record("b", &release(KeyTag::Char('x')), (0.0, 400.0));

        // a's viewport moved down since: its pointer follows the strip.
        let back = seats.hand_over("a", "A", ModifiersState::empty(), (0.0, 100.0));
        assert_eq!(back.release.map(|(_, actions)| actions), Some(Vec::new()));
        assert_eq!(
            back.restore,
            vec![
                PlannedAction::CursorMoved { x: 40.0, y: 130.0 },
                press(SHIFT),
                PlannedAction::MouseLeftButton { pressed: true },
            ]
        );
        assert_eq!(back.modifiers, Some(ModifiersState::SHIFT));
    }

    #[test]
    fn the_seated_agent_keeps_the_seat_without_a_handover() {
        let mut seats = AgentSeats::new();
        let _ = seats.hand_over("a", "A", ModifiersState::empty(), (0.0, 0.0));
        seats.record("a", &press(SHIFT), (0.0, 0.0));
        assert!(
            seats
                .hand_over("a", "A", ModifiersState::SHIFT, (0.0, 0.0))
                .is_empty()
        );
        assert_eq!(
            seats.seat("a").map(AgentSeat::held_keys),
            Some(&[SHIFT][..])
        );
    }

    #[test]
    fn every_agent_gets_its_own_participant_far_from_people() {
        let mut seats = AgentSeats::new();
        let a = seats.seat_mut("a", "A").peer;
        let b = seats.seat_mut("b", "B").peer;
        assert_ne!(a, b);
        assert_eq!(seats.seat_mut("a", "renamed").peer, a, "stable per agent");
        assert!(a > PeerId::MAX / 2 && b > PeerId::MAX / 2);
    }

    #[test]
    fn leaving_releases_only_what_is_still_on_the_shared_seat() {
        let mut seats = AgentSeats::new();
        let _ = seats.hand_over("a", "A", ModifiersState::empty(), (0.0, 0.0));
        seats.record("a", &press(SHIFT), (0.0, 0.0));
        let _ = seats.hand_over("b", "B", ModifiersState::SHIFT, (0.0, 0.0));
        seats.record("b", &press(KeyTag::Char('q')), (0.0, 0.0));

        let (_, suspended) = seats.leave("a").unwrap_or_else(|| panic!("a had a seat"));
        assert!(
            suspended.is_empty(),
            "a's Shift was released at the handover"
        );
        let (_, seated) = seats.leave("b").unwrap_or_else(|| panic!("b had a seat"));
        assert_eq!(seated, vec![release(KeyTag::Char('q'))]);
        assert_eq!(seats.seated(), None);
        assert!(seats.is_empty());
        assert!(seats.leave("b").is_none());
    }

    #[test]
    fn a_strip_has_at_most_one_agent() {
        let mut seats = AgentSeats::new();
        let strip = StripId(7);
        seats.seat_mut("a", "A").strip = Some(strip);
        assert_eq!(seats.strip_owner(strip), Some("a"));
        assert_eq!(seats.strip_owner(StripId(8)), None);
    }
}

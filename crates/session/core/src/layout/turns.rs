//! A turn asked on the canvas and the room it keeps for its answer (canvas.md §3): the turn's
//! view stands where the person asked, and the places the agent's steps in it make fill the
//! rectangle the person dragged, or stand beside the turn.
//!
//! A turn is named by the message that asked it (`<conversation>/messages/<message>`, as its
//! steps name it), never by its conversation: two asks in one conversation are two turns, each
//! with its own room. Where a turn stands is known when the person asks; a step the agent takes
//! before its turn's view stands goes where the turn will, by the message Chat posted for the ask.

use std::rc::Rc;

use serde_json::Value as Json;
use tilo_actions::Actor;

use super::{Asked, Busy, Layout, LayoutAction, LayoutActions, locate};

/// The view a turn stands on the canvas through (canvas.md §3): the person's line, the agent's
/// reply under it, its steps folded.
pub const TURN_VIEW: &str = "turn";

/// How wide a turn stands when the person only clicked: the measure an answer reads at.
const TURN_WIDTH: i64 = 520;
/// The most of a dragged rectangle a turn takes at its top: the rest is the room the agent's
/// views fill. The turn's view hugs its words up to it (canvas.md §7, "The card hugs what it
/// holds").
const TURN_HEIGHT: i64 = 240;
/// The most a turn stands when the person only clicked: there is no rectangle to keep to, so its
/// view hugs its words up to a window's own height, and a long answer reads whole rather than
/// cut at its last line.
const TURN_MOST: i64 = 480;
/// The gap between a turn at a rectangle's top and the room left under it: the canvas's
/// own gap between a place and the one under it, so a place filling the room stands where the
/// canvas would keep it and its caption stands clear of the turn.
const TURN_GAP: i64 = tilo_desktop_model::CANVAS_ROW_GAP as i64;
/// The least room under a turn that the agent's views fill: a rectangle shorter than this is
/// filled whole, and the turn's own view keeps its band over it.
const LEAST_ROOM: i64 = 160;

/// A rectangle on the canvas, in canvas pixels: its top-left corner and its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasRect {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

/// Where the places an agent makes in a turn stand (canvas.md §3, §6): the turn's own place, and
/// the room the person dragged for its answer while nothing has filled it yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TurnAnchor {
    /// The place the turn's view stands in.
    pub(super) place: u64,
    /// The rectangle the person dragged, under the turn: the first place the turn makes fills it.
    pub(super) room: Option<CanvasRect>,
    /// The conversation the turn was asked in, by its address.
    pub(super) conversation: String,
    /// The order the turns stood in: a later turn has a greater one.
    pub(super) stood: u64,
    /// The part of the canvas the person saw when they asked: what the turn's views stand beside
    /// it take free room inside.
    pub(super) seen: Option<CanvasRect>,
}

/// What a session reads which message Chat posted for the ask numbered so through, once Chat
/// answered: the message's id in its conversation.
pub type Posted = Rc<dyn Fn(u64) -> Option<String>>;

/// The name a turn goes by: the address of the message `message` of `conversation` that asked
/// it, as a step taken in the turn names what it stands beside.
#[must_use]
pub fn turn_key(conversation: &str, message: &str) -> String {
    format!("{}{MESSAGES}{}", conversation.trim(), message.trim())
}

/// What separates a conversation's address from its message's in a [`turn_key`].
const MESSAGES: &str = "/messages/";

/// The conversation and the message a [`turn_key`] names; `None` for anything else.
fn key_parts(key: &str) -> Option<(&str, &str)> {
    key.trim()
        .rsplit_once(MESSAGES)
        .filter(|(conversation, message)| !conversation.is_empty() && !message.is_empty())
}

/// The size a turn stands at for an ask at `at` of `size` (`None`: a click), and the room under it
/// a dragged rectangle leaves for the answer's views.
fn turn_geometry(
    at: (i64, i64),
    size: Option<(i64, i64)>,
    seen: Option<CanvasRect>,
) -> (i64, i64, Option<CanvasRect>) {
    let Some((width, height)) = size else {
        // At a click the turn hugs its answer up to its most, at an answer's measure as far as
        // the room to the edge of what the person sees allows; a rectangle sets its band.
        return (
            super::width_in_view(at.0, TURN_WIDTH, seen),
            TURN_MOST,
            None,
        );
    };
    let band = TURN_HEIGHT.min(height);
    let left = height.saturating_sub(band).saturating_sub(TURN_GAP);
    // The room under the turn's band is what the answer's views fill; a rectangle with too little
    // of it under the band leaves them to stand beside the turn.
    let room = (left >= LEAST_ROOM).then_some(CanvasRect {
        x: at.0,
        y: at.1.saturating_add(band).saturating_add(TURN_GAP),
        width,
        height: left,
    });
    (width, band, room)
}

/// Where a step of a turn stands what it shows (canvas.md §3): in the room the person dragged for
/// the answer, or next to the turn — its place once it stands, else the rectangle it will take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TurnRoom {
    /// The room under the turn, which this step takes.
    Fill(CanvasRect),
    /// Beside the turn's place, inside what the person saw when they asked, when that is known.
    Beside(u64, Option<CanvasRect>),
    /// Beside where the turn will stand once its view does, inside what the person saw.
    BesideAsked(CanvasRect, Option<CanvasRect>),
}

/// An ask sent from the canvas whose turn does not stand yet: where it stands once Chat said
/// which message it posted for it ([`Layout::stand_asked`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AwaitedTurn {
    /// Who asked.
    asked: Actor,
    /// The number the session's send goes by ([`Ask`]).
    sent: u64,
    /// The strip the caret was in, else the canvas.
    strip: Option<u64>,
    /// Where they asked.
    at: (i64, i64),
    /// The rectangle they dragged, when they dragged one.
    size: Option<(i64, i64)>,
    /// What they saw of the canvas when they asked.
    seen: Option<CanvasRect>,
    /// A step of the turn filled the room under it before the turn stood.
    room_taken: bool,
}

/// What a session reads an ask's turn back from: the turn the message Chat posted for the send
/// numbered so opened, once Chat answered which message that is and its record holds it, with
/// the resources the session publishes from then on: the turn, and the conversation it is in.
pub type FindTurn<'a> = &'a dyn Fn(u64) -> Option<(Asked, Vec<tilo_actions::ResourceSpec>)>;

impl Layout {
    /// Keep the ask the person sent from the canvas, numbered `sent`, until its turn is known:
    /// the turn then stands where they asked ([`Self::stand_asked`]).
    pub(super) fn await_turn(
        &self,
        asked: &Actor,
        sent: u64,
        strip: Option<u64>,
        (at, size): ((i64, i64), Option<(i64, i64)>),
        seen: Option<CanvasRect>,
    ) {
        self.awaiting.borrow_mut().push(AwaitedTurn {
            asked: asked.to_owned(),
            sent,
            strip,
            at,
            size,
            seen,
            room_taken: false,
        });
    }

    /// Read which message Chat posted for each ask through `posted`: what names an ask's turn
    /// before its view stands, so a step of it goes where the turn will.
    pub fn know_posted(&self, posted: Posted) {
        *self.posted.borrow_mut() = Some(posted);
    }

    /// Whether an ask sent from the canvas still waits for its turn.
    #[must_use]
    pub fn awaits_turns(&self) -> bool {
        !self.awaiting.borrow().is_empty()
    }

    /// Stand each turn an ask from the canvas opened that `find` now knows, where the person
    /// asked (canvas.md §3); the others keep waiting. Answers how many stood.
    pub fn stand_asked(&mut self, find: FindTurn<'_>, busy: Busy) -> usize {
        let awaiting = std::mem::take(&mut *self.awaiting.borrow_mut());
        let mut stood: usize = 0;
        for ask in awaiting {
            let Some((turn, published)) = find(ask.sent) else {
                self.awaiting.borrow_mut().push(ask);
                continue;
            };
            // Published from this pass on, and placed now: the API's tree takes them in later.
            for resource in &published {
                self.catalog.borrow_mut().remember_resource(resource);
            }
            match self.place_turn(&ask, &turn, busy) {
                Ok(_) => stood = stood.saturating_add(1),
                Err(why) => {
                    tracing::warn!(%why, turn = %turn.turn, "the asked turn does not stand");
                }
            }
        }
        stood
    }

    /// Stand the turn an ask opened where the person asked (canvas.md §3): its `turn` view at the
    /// caret — at the measure an answer reads at for a click, or across the top of the rectangle
    /// they dragged — and the turn kept as its conversation's anchor, so the places the agent's
    /// steps in it make fill the rest of that rectangle, or stand beside the turn. One arranging
    /// step of the person's; the conversation itself stays where it is, Chat's channel, and is
    /// not placed.
    fn place_turn(
        &mut self,
        ask: &AwaitedTurn,
        turn: &Asked,
        busy: Busy,
    ) -> Result<Option<u64>, String> {
        let (asked, at) = (&ask.asked, ask.at);
        if let Some(held) = self.place_of(&turn.turn) {
            self.bring_forward(asked, held, busy)?;
            return Ok(Some(held));
        }
        let (width, height, room) = turn_geometry(at, ask.size, ask.seen);
        // Asked by a click, the turn stands at the click, narrowed to the free area before the
        // next place to its right as a thing found at a click is; with no area free there, in the free
        // spot nearest the click that the person sees, never past the edge of the screen with the
        // camera panning after it.
        let (at, width, height) = if ask.size.is_none() {
            self.spot_at_click(at, width, height, ask.seen)
        } else {
            (at, width, height)
        };
        // A step of the turn taken before the turn stood filled the room already.
        let room = room.filter(|_| !ask.room_taken);
        let surface = self.surface_at_strip(ask.strip)?;
        let action = LayoutAction::Place {
            workspace: surface.workspace,
            row: surface.row,
            strip: surface.strip,
            place: surface.place,
            resource: turn.turn.clone(),
            view: TURN_VIEW.to_owned(),
            x: at.0,
            y: at.1,
            width,
            height,
            float: None,
            // A turn is drawn as tall as its words, up to its height (canvas.md §7): the layout
            // takes the height it is drawn at, so the canvas under a short answer stays free.
            hugs: true,
        };
        self.ask(asked, action, busy)?;
        self.opened_as
            .borrow_mut()
            .insert(turn.opened_as.clone(), turn.conversation.clone());
        let stood = self
            .turns
            .borrow()
            .values()
            .map(|anchor| anchor.stood.saturating_add(1))
            .max()
            .unwrap_or_default();
        self.turns.borrow_mut().insert(
            turn_key(&turn.conversation, &turn.message),
            TurnAnchor {
                place: surface.place,
                room,
                conversation: turn.conversation.clone(),
                stood,
                seen: ask.seen,
            },
        );
        Ok(Some(surface.place))
    }

    /// The latest turn asked on the canvas whose conversation `resource` is, as its app publishes
    /// it, while the turn's place stands: by its [`turn_key`].
    pub(super) fn conversation_opened_as(&self, resource: &str) -> Option<String> {
        let conversation = self.opened_as.borrow().get(resource.trim())?.clone();
        let mut turns: Vec<(String, u64)> = self
            .turns
            .borrow()
            .iter()
            .filter(|(_, anchor)| anchor.conversation == conversation)
            .map(|(key, anchor)| (key.clone(), anchor.stood))
            .collect();
        turns.sort_by_key(|(_, stood)| std::cmp::Reverse(*stood));
        turns
            .into_iter()
            .map(|(key, _)| key)
            .find(|key| self.turn_of(key).is_some())
    }

    /// Open the whole conversation the turn `turn` (its address) was asked in, beside the turn:
    /// what the turn's place offers in its handle (canvas.md §7, "the conversation is reached
    /// from the handle"). Chat's own page of it, through the system's `open`, which stands it
    /// beside the turn rather than in the room the turn keeps for its answer, or brings it
    /// forward when it stands already.
    ///
    /// # Errors
    /// The turn stands nowhere in the layout, or no conversation is known for it.
    pub fn open_conversation_of(
        &mut self,
        asked: &Actor,
        turn: &str,
    ) -> Result<Option<u64>, String> {
        let place = self
            .place_of(turn.trim())
            .ok_or_else(|| format!("the answer {turn} stands nowhere in the layout"))?;
        // The turn's own key names it to `open_place`, which then stands its conversation beside
        // the turn rather than in the room the turn keeps for its answer.
        let (key, conversation) = self
            .turns
            .borrow()
            .iter()
            .find(|(_, anchor)| anchor.place == place)
            .map(|(key, anchor)| (key.clone(), anchor.conversation.clone()))
            .ok_or_else(|| format!("no conversation is known for the answer {turn}"))?;
        let opened_as = self
            .opened_as
            .borrow()
            .iter()
            .find(|(_, held)| **held == conversation)
            .map(|(opened_as, _)| opened_as.clone())
            .ok_or_else(|| format!("the conversation {conversation} is not published"))?;
        self.open_place(asked, &opened_as, "", &key)
    }

    /// Whether `resource` is, as its app publishes it, the conversation the turn `key` names was
    /// asked in: the whole conversation, which the turn's room is not for.
    pub(super) fn is_conversation_of(&self, resource: &str, key: &str) -> bool {
        let Some((conversation, _)) = key_parts(key) else {
            return false;
        };
        self.opened_as
            .borrow()
            .get(resource.trim())
            .is_some_and(|opened| opened == conversation)
    }

    /// The turn asked on the canvas that `key` names ([`turn_key`]), while its place stands.
    pub(super) fn turn_of(&self, key: &str) -> Option<TurnAnchor> {
        let turn = self.turns.borrow().get(key.trim())?.clone();
        self.handle
            .read(|tree| locate(tree, turn.place).is_some())
            .then_some(turn)
    }

    /// The ask still waiting for its turn's view that `key` names ([`turn_key`]), by where it is
    /// among the waiting asks: the one Chat posted that message for.
    fn awaited_of(&self, key: &str) -> Option<usize> {
        let (_, message) = key_parts(key)?;
        let posted = self.posted.borrow().clone()?;
        self.awaiting
            .borrow()
            .iter()
            .position(|ask| posted(ask.sent).as_deref() == Some(message))
    }

    /// Where a step of the turn `key` names ([`turn_key`]) stands what it shows, taking the turn's
    /// room while it is free: the turn's own, kept from when the person asked, whether its view
    /// stands yet or not. `None` when `key` names no turn asked on the canvas.
    pub(super) fn take_turn_room(&self, key: &str) -> Option<TurnRoom> {
        let key = key.trim();
        if let Some(turn) = self.turn_of(key) {
            let Some(room) = turn.room else {
                return Some(TurnRoom::Beside(turn.place, turn.seen));
            };
            if let Some(held) = self.turns.borrow_mut().get_mut(key) {
                held.room = None;
            }
            return Some(TurnRoom::Fill(room));
        }
        let at = self.awaited_of(key)?;
        let mut awaiting = self.awaiting.borrow_mut();
        let ask = awaiting.get_mut(at)?;
        let (width, height, room) = turn_geometry(ask.at, ask.size, ask.seen);
        if let Some(room) = room.filter(|_| !ask.room_taken) {
            ask.room_taken = true;
            return Some(TurnRoom::Fill(room));
        }
        Some(TurnRoom::BesideAsked(
            CanvasRect {
                x: ask.at.0,
                y: ask.at.1,
                width,
                height,
            },
            ask.seen,
        ))
    }
}

impl Layout {
    /// The turn asked on the canvas that `action` would cover, by its [`turn_key`], while the
    /// turn stands (canvas.md §3): a `show` that re-points the turn's own place, or a `place` that
    /// joins the strip the turn stands in (`names_strip`: it named one). `None` for a step that
    /// covers no turn.
    pub(super) fn turn_covered_by(
        &self,
        action: &LayoutAction,
        names_strip: bool,
    ) -> Option<String> {
        let turns: Vec<(String, u64)> = self
            .turns
            .borrow()
            .iter()
            .map(|(key, anchor)| (key.clone(), anchor.place))
            .collect();
        self.handle.read(|tree| {
            let strip_of = |place: u64| {
                let (workspace, row, group, _) = locate(tree, place)?;
                Some(
                    tree.workspaces
                        .get(workspace)?
                        .strips
                        .get(row)?
                        .window_groups
                        .get(group)?
                        .id,
                )
            };
            turns.into_iter().find_map(|(key, turn)| {
                let covered = match action {
                    LayoutAction::Show { place, .. } => *place == turn,
                    LayoutAction::Place { strip, .. } if names_strip => {
                        strip_of(turn) == Some(*strip)
                    }
                    _ => false,
                };
                (covered && locate(tree, turn).is_some()).then_some(key)
            })
        })
    }
}

impl LayoutActions {
    /// Keep a turn asked on the canvas its own place (canvas.md §3): a step that would show
    /// something else in the turn's place, or join it as a tab, stands what it shows where the
    /// turn's answer goes instead — the room of the rectangle the person dragged while it is
    /// free, else beside the turn — and the turn's view stays. Whether it did; `false` leaves any
    /// other step as it is.
    pub(super) fn kept_turn(
        &self,
        asked: &Actor,
        arguments: &serde_json::Map<String, Json>,
        action: &LayoutAction,
    ) -> Result<bool, tilo_actions::ApiError> {
        let names_strip = arguments.get("strip").is_some_and(|strip| !strip.is_null());
        let (LayoutAction::Show { resource, view, .. }
        | LayoutAction::Place { resource, view, .. }) = action
        else {
            return Ok(false);
        };
        let covered = self.layout.borrow().turn_covered_by(action, names_strip);
        let Some(turn) = covered else {
            return Ok(false);
        };
        self.layout
            .borrow_mut()
            .open_place(asked, resource, view, &turn)
            .map_err(|why| tilo_actions::ApiError::Unavailable {
                code: "nowhere_to_place".to_owned(),
                reason: why,
            })?;
        Ok(true)
    }
}

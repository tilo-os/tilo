//! A turn asked on the canvas and the room it keeps for its answer (canvas.md §3): the turn's
//! view stands where the person asked, and the places the agent's steps in it make fill the
//! rectangle the person dragged, or stand beside the turn.

use serde_json::Value as Json;

use super::{Arrangement, ArrangementAction, Asked, Busy, Door, locate};

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
/// The gap between a turn standing at a rectangle's top and the room left under it: the canvas's
/// own gap between places, so a place filling the room stands where the canvas would keep it.
const TURN_GAP: i64 = tilo_desktop_model::CANVAS_GAP as i64;
/// The least room under a turn that the agent's views fill: a rectangle shorter than this is
/// filled whole, and the turn's own view keeps its band over it.
const LEAST_ROOM: i64 = 160;

/// A rectangle on the canvas, in canvas pixels: its top-left corner and its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CanvasRect {
    pub(super) x: i64,
    pub(super) y: i64,
    pub(super) width: i64,
    pub(super) height: i64,
}

/// Where the places an agent makes in a turn stand (canvas.md §3, §6): the turn's own place, and
/// the room the person dragged for its answer while nothing has filled it yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TurnAnchor {
    /// The place the turn's view stands in.
    pub(super) place: u64,
    /// The rectangle the person dragged, under the turn: the first place the turn makes fills it.
    pub(super) room: Option<CanvasRect>,
}

/// An ask sent from the canvas whose turn does not stand yet: where it stands once Chat said
/// which message it posted for it ([`Arrangement::stand_asked`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AwaitedTurn {
    /// Who asked.
    asked: String,
    /// The number the session's send goes by ([`Ask`]).
    sent: u64,
    /// The strip the caret was in, else the canvas.
    strip: Option<u64>,
    /// Where they asked.
    at: (i64, i64),
    /// The rectangle they dragged, when they dragged one.
    size: Option<(i64, i64)>,
}

/// What a session reads an ask's turn back from: the turn the message Chat posted for the send
/// numbered so opened, once Chat answered which message that is and its record holds it, with
/// the resources the session publishes from then on: the turn, and the conversation it is in.
pub type FindTurn<'a> = &'a dyn Fn(u64) -> Option<(Asked, Vec<tilo_actions::ResourceSpec>)>;

impl Arrangement {
    /// Keep the ask the person sent from the canvas, numbered `sent`, until its turn is known:
    /// the turn then stands where they asked ([`Self::stand_asked`]).
    pub(super) fn await_turn(
        &self,
        asked: &str,
        sent: u64,
        strip: Option<u64>,
        at: (i64, i64),
        size: Option<(i64, i64)>,
    ) {
        self.awaiting.borrow_mut().push(AwaitedTurn {
            asked: asked.to_owned(),
            sent,
            strip,
            at,
            size,
        });
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
            // Published from this pass on, and placed now: the door's tree takes them in later.
            for resource in &published {
                self.catalog.borrow_mut().remember_resource(resource);
            }
            match self.place_turn(&ask.asked, &turn, ask.strip, ask.at, ask.size, busy) {
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
    pub(super) fn place_turn(
        &mut self,
        asked: &str,
        turn: &Asked,
        strip: Option<u64>,
        at: (i64, i64),
        size: Option<(i64, i64)>,
        busy: Busy,
    ) -> Result<Option<u64>, String> {
        if let Some(held) = self.place_of(&turn.turn) {
            self.bring_forward(asked, held, busy)?;
            return Ok(Some(held));
        }
        let (width, height, room) = match size {
            None => (TURN_WIDTH, TURN_MOST, None),
            Some((width, height)) => {
                let band = TURN_HEIGHT.min(height);
                let left = height.saturating_sub(band).saturating_sub(TURN_GAP);
                // The room under the turn's band is what the answer's views fill; a rectangle
                // with too little of it under the band leaves them to stand beside the turn.
                let room = (left >= LEAST_ROOM).then_some(CanvasRect {
                    x: at.0,
                    y: at.1.saturating_add(band).saturating_add(TURN_GAP),
                    width,
                    height: left,
                });
                (width, band, room)
            }
        };
        let surface = self.surface_at_strip(strip)?;
        let action = ArrangementAction::Place {
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
        };
        self.ask(asked, action, busy)?;
        self.opened_as
            .borrow_mut()
            .insert(turn.opened_as.clone(), turn.conversation.clone());
        self.turns.borrow_mut().insert(
            turn.conversation.clone(),
            TurnAnchor {
                place: surface.place,
                room,
            },
        );
        Ok(Some(surface.place))
    }

    /// The conversation of a turn asked on the canvas that `resource` is, as its app publishes it,
    /// while the turn's place stands: by the address the turn's anchor is kept under.
    pub(super) fn conversation_opened_as(&self, resource: &str) -> Option<String> {
        let conversation = self.opened_as.borrow().get(resource.trim())?.clone();
        self.turn_of(&conversation).map(|_| conversation)
    }

    /// Open the whole conversation the turn `turn` (its address) was asked in, beside the turn:
    /// what the turn's place offers in its handle (canvas.md §7, "the conversation is reached
    /// from the handle"). Chat's own page of it, through the system's `open`, which stands it
    /// beside the turn rather than in the room the turn keeps for its answer, or brings it
    /// forward when it stands already.
    ///
    /// # Errors
    /// The turn stands nowhere on the desk, or no conversation is known for it.
    pub fn open_conversation_of(&mut self, asked: &str, turn: &str) -> Result<Option<u64>, String> {
        let place = self
            .place_of(turn.trim())
            .ok_or_else(|| format!("the answer {turn} stands nowhere on the desk"))?;
        let conversation = self
            .turns
            .borrow()
            .iter()
            .find(|(_, anchor)| anchor.place == place)
            .map(|(conversation, _)| conversation.clone())
            .ok_or_else(|| format!("no conversation is known for the answer {turn}"))?;
        let opened_as = self
            .opened_as
            .borrow()
            .iter()
            .find(|(_, held)| **held == conversation)
            .map(|(opened_as, _)| opened_as.clone())
            .ok_or_else(|| format!("the conversation {conversation} is not published"))?;
        self.open_place(asked, &opened_as, "", &conversation)
    }

    /// The turn asked on the canvas in the conversation `beside` names, while its place stands.
    pub(super) fn turn_of(&self, beside: &str) -> Option<TurnAnchor> {
        let turn = *self.turns.borrow().get(beside.trim())?;
        self.handle
            .read(|tree| locate(tree, turn.place).is_some())
            .then_some(turn)
    }
}

impl Arrangement {
    /// The conversation of the turn asked on the canvas that `action` would cover, while the
    /// turn stands (canvas.md §3): a `show` that re-points the turn's own place, or a `place` that
    /// joins the strip the turn stands in (`names_strip`: it named one). `None` for a step that
    /// covers no turn.
    pub(super) fn turn_covered_by(
        &self,
        action: &ArrangementAction,
        names_strip: bool,
    ) -> Option<String> {
        let turns: Vec<(String, u64)> = self
            .turns
            .borrow()
            .iter()
            .map(|(conversation, anchor)| (conversation.clone(), anchor.place))
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
            turns.into_iter().find_map(|(conversation, turn)| {
                let covered = match action {
                    ArrangementAction::Show { place, .. } => *place == turn,
                    ArrangementAction::Place { strip, .. } if names_strip => {
                        strip_of(turn) == Some(*strip)
                    }
                    _ => false,
                };
                (covered && locate(tree, turn).is_some()).then_some(conversation)
            })
        })
    }
}

impl Door {
    /// Keep a turn asked on the canvas its own place (canvas.md §3): a step that would show
    /// something else in the turn's place, or join it as a tab, stands what it shows where the
    /// turn's answer goes instead — the room of the rectangle the person dragged while it is
    /// free, else beside the turn — and the turn's view stays. Whether it did; `false` leaves any
    /// other step as it is.
    pub(super) fn kept_turn(
        &self,
        asked: &str,
        arguments: &serde_json::Map<String, Json>,
        action: &ArrangementAction,
    ) -> Result<bool, tilo_actions::DoorError> {
        let names_strip = arguments.get("strip").is_some_and(|strip| !strip.is_null());
        let (ArrangementAction::Show { resource, view, .. }
        | ArrangementAction::Place { resource, view, .. }) = action
        else {
            return Ok(false);
        };
        let covered = self
            .arrangement
            .borrow()
            .turn_covered_by(action, names_strip);
        let Some(conversation) = covered else {
            return Ok(false);
        };
        self.arrangement
            .borrow_mut()
            .open_place(asked, resource, view, &conversation)
            .map_err(|why| tilo_actions::DoorError::Unavailable {
                code: "nowhere_to_place".to_owned(),
                reason: why,
            })?;
        Ok(true)
    }
}

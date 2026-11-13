//! A turn asked on the canvas and the room it keeps for its answer (canvas.md §3): the turn's
//! view stands where the person asked, and the places the agent's steps in it make fill the
//! rectangle the person dragged, or stand beside the turn.

use super::*;

/// The view a turn stands on the canvas through (canvas.md §3): the person's line, the agent's
/// reply under it, its steps folded.
pub const TURN_VIEW: &str = "turn";

/// How wide a turn stands when the person only clicked: the measure an answer reads at.
const TURN_WIDTH: i64 = 520;
/// How tall a turn stands when the person only clicked, and the most of a dragged rectangle it
/// takes at its top: the rest is the room the agent's views fill.
const TURN_HEIGHT: i64 = 240;
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

impl Arrangement {
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
            None => (TURN_WIDTH, TURN_HEIGHT, None),
            Some((width, height)) => {
                let band = TURN_HEIGHT.min(height);
                let left = height - band - TURN_GAP;
                // The room under the turn's band is what the answer's views fill; a rectangle
                // with too little of it under the band leaves them to stand beside the turn.
                let room = (left >= LEAST_ROOM).then_some(CanvasRect {
                    x: at.0,
                    y: at.1 + band + TURN_GAP,
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
        self.turns.borrow_mut().insert(
            turn.conversation.clone(),
            TurnAnchor {
                place: surface.place,
                room,
            },
        );
        Ok(Some(surface.place))
    }

    /// The turn asked on the canvas in the conversation `beside` names, while its place stands.
    pub(super) fn turn_of(&self, beside: &str) -> Option<TurnAnchor> {
        let turn = *self.turns.borrow().get(beside.trim())?;
        self.handle
            .read(|tree| locate(tree, turn.place).is_some())
            .then_some(turn)
    }
}

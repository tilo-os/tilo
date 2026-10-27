//! What the capture scenarios stage: a table with the person at it, and a
//! hand under way. Never written into a real room's table.
//!
//! The table is played by the app's own rules — a [`Replica`] applies every
//! move through the same door a person's does — so what a frame shows is a
//! table the game could have reached. It is then staged as the record's
//! start, since a capture cannot wait twelve seconds into a turn.

use ui::prelude::*;

use crate::game::Table;

/// The scenarios a capture can ask for.
pub(crate) const SCENARIOS: &[&str] = &["waiting", "your-turn"];

/// How far into the waiting player's turn a capture opens: enough gone that
/// the fold for them reads as counting down, not as a turn just begun.
const INTO_TURN_MS: i64 = 12_000;

/// How many moves the staging may take before it shows the table it reached.
const MOVES: usize = 16;

/// How long before the capture the first of those moves is made: each one a
/// second after the last, far enough back that the turn has begun.
const SPAN_MS: i64 = 16_000;

/// The table a scenario shows: the person seated with one other player, a
/// hand dealt, and the turn where the scenario wants it.
///
/// `waiting` has the other player to act, so the person is offered the fold
/// for them; `your-turn` has the person to act, with their own moves.
pub(crate) fn table(scenario: &str, me: &str) -> Table {
    let me = me.to_owned();
    let now = ui::time::unix_now_ms_i64();
    // A capture's people are called by their ids, and nobody else is in the
    // room to name this one.
    let other = "Grace";
    let mine = scenario == "your-turn";
    let mut replica = Replica::<Table>::new();
    let mut at = now.saturating_sub(SPAN_MS);
    for _ in 0..MOVES {
        let Ok(table) = replica.state() else {
            break;
        };
        let next: Option<(String, Invoke<Table>)> = if !table.players.contains_key(&me) {
            Some((me.clone(), Table::SIT.with(1)))
        } else if !table.players.contains_key(other) {
            Some((other.to_owned(), Table::SIT.with(3)))
        } else if table.hand.street.in_play() {
            match table.hand.actor().map(|p| p.player.clone()) {
                // The turn is where the scenario wants it.
                Some(actor) if (actor == me) == mine => None,
                Some(actor) => Some((actor, Table::CALL.invoke())),
                None => None,
            }
        } else {
            table
                .dealer()
                .map(|dealer| (dealer.to_owned(), Table::DEAL.invoke()))
        };
        let Some((who, invoke)) = next else {
            break;
        };
        at = at.saturating_add(1_000);
        replica = replica.as_author(&who);
        replica.set_now_ms(at);
        // A move the table refuses is a scenario that cannot be staged:
        // what it reached before is what the frame shows.
        if replica.apply(invoke).is_err() {
            break;
        }
    }
    let mut table = replica.state().unwrap_or_default();
    // The turn began `INTO_TURN_MS` before the capture.
    if table.hand.street.in_play() {
        table.hand.since_ms = now.saturating_sub(INTO_TURN_MS);
    }
    table
}

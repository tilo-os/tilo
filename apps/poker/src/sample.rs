//! What the capture scenarios stage: a table with the person at it, and a hand under way.
//! Never written into a real workspace's table. It is then staged as the record's start,
//! since a capture cannot wait twelve seconds into a turn.

use ui::prelude::*;

use crate::game::Table;

pub(crate) const SCENARIOS: &[&str] = &["waiting", "your-turn"];

const INTO_TURN_MS: i64 = 12_000;

const MOVES: usize = 16;

const SPAN_MS: i64 = 16_000;

/// The table a scenario shows: the person seated with one other player, a hand dealt, and
/// the turn where the scenario wants it.
pub(crate) fn table(scenario: &str, me: &str) -> Table {
    let me = me.to_owned();
    let now = ui::time::unix_now_ms_i64();
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
        // A move the table refuses is a scenario that cannot be staged: what it reached
        // before is what the frame shows.
        if replica.apply(invoke).is_err() {
            break;
        }
    }
    let mut table = replica.state().unwrap_or_default();
    if table.hand.street.in_play() {
        table.hand.since_ms = now.saturating_sub(INTO_TURN_MS);
    }
    table
}

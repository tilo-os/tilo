//! The table: who sits where, and the hand being played.
//!
//! Everything here is the shared record. Every move is an `#[action]`, so a
//! person's click, an agent's call and a script land on the same rules, and
//! every replica that applies the move reaches the same table.
//!
//! Collaboration shapes the model:
//!
//! - Players are keyed by participant id, so two people sitting down at the
//!   same moment both keep their entry. Where both asked for the same seat,
//!   [`Table::seating`] gives the later one the next free seat, the same way
//!   on every replica.
//! - Only the player whose turn it is may act, and only the dealer
//!   ([`Table::dealer`]) may deal, so two people never write the same hand at
//!   once.
//! - Randomness comes from the action ([`ActionCx::uuid`]): the deal carries
//!   its own shuffle, and replaying it deals the same cards.
//!
//! The deck and every player's cards are in the shared record. The view
//! shows a player only their own cards until the showdown, and History
//! hides the cards of a hand in progress.

use std::collections::BTreeMap;

use ui::prelude::*;

use crate::cards;

/// Seats at the table.
pub const SEATS: usize = 6;
/// Chips a player sits down with, and rebuys to.
pub const BUY_IN: i64 = 1000;
/// The small blind; the big blind is twice it.
pub const SMALL_BLIND: i64 = 5;
/// The big blind.
pub const BIG_BLIND: i64 = SMALL_BLIND * 2;
/// How long a player may take before anyone else may fold their hand.
pub const TURN_MS: i64 = 30_000;
/// How long after a hand ends anyone, not only the dealer, may deal the next.
pub const DEAL_GRACE_MS: i64 = 15_000;

/// Someone who sat down.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Player {
    /// The seat they asked for (0 to [`SEATS`] − 1).
    pub seat: u8,
    /// Chips in front of them between hands.
    pub chips: i64,
    /// When they sat down: the earlier of two people asking for one seat
    /// gets it.
    pub joined_ms: i64,
}

/// Where a hand is.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Street {
    /// No hand has been dealt yet.
    #[default]
    Idle,
    Preflop,
    Flop,
    Turn,
    River,
    /// The hand is over: the pots have been paid.
    Done,
}

impl Street {
    /// Whether betting is going on.
    #[must_use]
    pub const fn in_play(self) -> bool {
        matches!(self, Self::Preflop | Self::Flop | Self::Turn | Self::River)
    }

    /// The Fluent id naming the street.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Idle => "poker-street-idle",
            Self::Preflop => "poker-street-preflop",
            Self::Flop => "poker-street-flop",
            Self::Turn => "poker-street-turn",
            Self::River => "poker-street-river",
            Self::Done => "poker-street-done",
        }
    }
}

/// What a player did last in the hand.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    #[default]
    None,
    SmallBlind,
    BigBlind,
    Check,
    Call,
    Bet,
    Raise,
    AllIn,
    Fold,
}

impl Move {
    /// The Fluent id naming the move, when there is one to show.
    #[must_use]
    pub const fn message(self) -> Option<&'static str> {
        Some(match self {
            Self::None => return None,
            Self::SmallBlind => "poker-move-small-blind",
            Self::BigBlind => "poker-move-big-blind",
            Self::Check => "poker-move-check",
            Self::Call => "poker-move-call",
            Self::Bet => "poker-move-bet",
            Self::Raise => "poker-move-raise",
            Self::AllIn => "poker-move-all-in",
            Self::Fold => "poker-move-fold",
        })
    }
}

/// One player's part in a hand.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct InHand {
    /// The participant.
    pub player: String,
    /// The seat they play from.
    pub seat: u8,
    /// Chips behind: not yet bet.
    pub stack: i64,
    /// Chips bet on this street.
    pub bet: i64,
    /// Chips put in over the whole hand.
    pub committed: i64,
    /// Their two cards.
    pub cards: Vec<u8>,
    pub folded: bool,
    /// Whether they have acted since the last bet or raise.
    pub acted: bool,
    /// Whether their cards are shown to everyone.
    pub shown: bool,
    pub last: Move,
}

impl InHand {
    /// Whether they can still bet: in the hand with chips behind.
    #[must_use]
    pub fn can_act(&self) -> bool {
        !self.folded && self.stack > 0
    }
}

/// What a player won at the end of a hand.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Award {
    pub player: String,
    pub amount: i64,
    /// The Fluent id naming their hand; empty when everyone else folded.
    pub category: String,
}

/// The hand being played, or the last one.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Hand {
    /// Counts from 1; 0 before the first deal.
    pub number: u64,
    pub street: Street,
    /// The players dealt in, in seat order.
    pub players: Vec<InHand>,
    /// The position (into `players`) of the dealer button.
    pub button: u32,
    /// The cards not dealt yet.
    pub deck: Vec<u8>,
    /// The community cards.
    pub board: Vec<u8>,
    /// The position of the player to act, while betting goes on.
    pub to_act: Option<u32>,
    /// The highest bet on this street.
    pub current_bet: i64,
    /// The smallest raise allowed: the size of the last bet or raise.
    pub min_raise: i64,
    /// When the player to act was given the turn, or when the hand ended.
    pub since_ms: i64,
    pub awards: Vec<Award>,
}

impl Hand {
    /// Chips in the middle: everything put in, this street's bets included.
    #[must_use]
    pub fn pot(&self) -> i64 {
        self.players.iter().map(|p| p.committed).sum()
    }

    /// The position of `player` in the hand.
    #[must_use]
    pub fn position(&self, player: &str) -> Option<usize> {
        self.players.iter().position(|p| p.player == player)
    }

    /// The player to act.
    #[must_use]
    pub fn actor(&self) -> Option<&InHand> {
        self.to_act
            .and_then(|at| self.players.get(usize::try_from(at).ok()?))
    }

    /// Whether `player` is dealt in and has not folded.
    #[must_use]
    pub fn is_live(&self, player: &str) -> bool {
        self.street.in_play() && self.players.iter().any(|p| p.player == player && !p.folded)
    }

    /// What `player` needs to put in to call.
    #[must_use]
    pub fn to_call(&self, player: &str) -> i64 {
        self.position(player)
            .and_then(|at| self.players.get(at))
            .map_or(0, |p| {
                self.current_bet.saturating_sub(p.bet).min(p.stack).max(0)
            })
    }

    /// The smallest total bet `player` may raise to, and the largest (all in).
    #[must_use]
    pub fn raise_range(&self, player: &str) -> Option<(i64, i64)> {
        let p = self.players.get(self.position(player)?)?;
        let most = p.bet.saturating_add(p.stack);
        if most <= self.current_bet {
            return None;
        }
        let least = self
            .current_bet
            .saturating_add(self.min_raise.max(BIG_BLIND))
            .min(most);
        Some((least, most))
    }

    fn next_from(&self, from: usize, wanted: impl Fn(&InHand) -> bool) -> Option<usize> {
        let n = self.players.len();
        (1..=n)
            .map(|step| from.saturating_add(step).checked_rem(n).unwrap_or(0))
            .find(|at| self.players.get(*at).is_some_and(&wanted))
    }

    fn live(&self) -> usize {
        self.players.iter().filter(|p| !p.folded).count()
    }

    fn deal_card(&mut self) -> u8 {
        self.deck.pop().unwrap_or(0)
    }

    fn put_in(&mut self, at: usize, amount: i64) -> i64 {
        let Some(p) = self.players.get_mut(at) else {
            return 0;
        };
        let amount = amount.min(p.stack).max(0);
        p.stack = p.stack.saturating_sub(amount);
        p.bet = p.bet.saturating_add(amount);
        p.committed = p.committed.saturating_add(amount);
        amount
    }

    fn give_turn(&mut self, at: Option<usize>, now_ms: i64) {
        self.to_act = at.and_then(|at| u32::try_from(at).ok());
        self.since_ms = now_ms;
    }

    /// Move the hand on after the player at `at` acted.
    fn advance(&mut self, at: usize, now_ms: i64) {
        if self.live() <= 1 {
            self.finish(now_ms);
            return;
        }
        let current = self.current_bet;
        let next = self.next_from(at, |p| p.can_act() && (!p.acted || p.bet < current));
        if next.is_some() {
            self.give_turn(next, now_ms);
            return;
        }
        self.next_street(now_ms);
    }

    /// Close the street's betting and deal the next one, or settle.
    fn next_street(&mut self, now_ms: i64) {
        loop {
            for p in &mut self.players {
                p.bet = 0;
                p.acted = false;
                if p.last != Move::Fold && p.last != Move::AllIn {
                    p.last = Move::None;
                }
            }
            self.current_bet = 0;
            self.min_raise = BIG_BLIND;
            self.street = match self.street {
                Street::Preflop => {
                    let _burn = self.deal_card();
                    for _ in 0..3 {
                        let card = self.deal_card();
                        self.board.push(card);
                    }
                    Street::Flop
                }
                Street::Flop | Street::Turn => {
                    let _burn = self.deal_card();
                    let card = self.deal_card();
                    self.board.push(card);
                    if self.street == Street::Flop {
                        Street::Turn
                    } else {
                        Street::River
                    }
                }
                _ => {
                    self.finish(now_ms);
                    return;
                }
            };
            // Betting goes on only while two players can still bet.
            if self.players.iter().filter(|p| p.can_act()).count() >= 2 {
                let button = usize::try_from(self.button).unwrap_or(0);
                let first = self.next_from(button, InHand::can_act);
                self.give_turn(first, now_ms);
                return;
            }
        }
    }

    /// Pay the pots and end the hand.
    fn finish(&mut self, now_ms: i64) {
        self.to_act = None;
        self.since_ms = now_ms;
        let committed: Vec<i64> = self.players.iter().map(|p| p.committed).collect();
        let folded: Vec<bool> = self.players.iter().map(|p| p.folded).collect();
        let contested = self.live() > 1;
        let strengths: Vec<Option<cards::Strength>> = self
            .players
            .iter()
            .map(|p| {
                if p.folded || !contested {
                    return None;
                }
                let mut seven = p.cards.clone();
                seven.extend_from_slice(&self.board);
                cards::best(&seven)
            })
            .collect();
        let mut won = vec![0_i64; self.players.len()];
        let button = usize::try_from(self.button).unwrap_or(0);
        for pot in cards::pots(&committed, &folded) {
            let best = pot
                .eligible
                .iter()
                .filter_map(|at| strengths.get(*at).copied().flatten())
                .max();
            let winners: Vec<usize> = pot
                .eligible
                .iter()
                .copied()
                .filter(|at| !contested || strengths.get(*at).copied().flatten() == best)
                .collect();
            let Ok(count) = i64::try_from(winners.len()) else {
                continue;
            };
            if count == 0 {
                continue;
            }
            let share = pot.amount.checked_div(count).unwrap_or(0);
            let mut odd = pot.amount.checked_rem(count).unwrap_or(0);
            // The odd chips go to the first winners left of the button.
            let n = self.players.len();
            let mut order = winners.clone();
            order.sort_by_key(|at| {
                at.saturating_add(n)
                    .saturating_sub(button)
                    .saturating_sub(1)
                    .checked_rem(n)
                    .unwrap_or(0)
            });
            for at in order {
                let extra = i64::from(odd > 0);
                odd = odd.saturating_sub(1);
                if let Some(total) = won.get_mut(at) {
                    *total = total.saturating_add(share).saturating_add(extra);
                }
            }
        }
        self.awards.clear();
        for (at, p) in self.players.iter_mut().enumerate() {
            let amount = won.get(at).copied().unwrap_or(0);
            p.stack = p.stack.saturating_add(amount);
            p.bet = 0;
            if contested && !p.folded {
                p.shown = true;
            }
            if amount > 0 {
                let category = strengths
                    .get(at)
                    .copied()
                    .flatten()
                    .map(|s| s.category().message().to_owned())
                    .unwrap_or_default();
                self.awards.push(Award {
                    player: p.player.clone(),
                    amount,
                    category,
                });
            }
        }
        self.street = Street::Done;
    }
}

/// The position after `at` among `n`, going round.
fn after(at: usize, n: usize) -> usize {
    at.saturating_add(1).checked_rem(n).unwrap_or(0)
}

/// The poker table.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Table {
    /// Everyone sitting at the table, by participant id.
    pub players: BTreeMap<String, Player>,
    pub hand: Hand,
    /// The seat the button was on for the last hand.
    pub button_seat: u8,
}

impl Table {
    /// Who sits in each seat. Two people who asked for one seat at the same
    /// time are seated in the order they sat down; the later one takes the
    /// next free seat to the left.
    #[must_use]
    pub fn seating(&self) -> [Option<String>; SEATS] {
        let mut order: Vec<(&String, &Player)> = self.players.iter().collect();
        order.sort_by(|a, b| (a.1.joined_ms, a.0).cmp(&(b.1.joined_ms, b.0)));
        let mut seats: [Option<String>; SEATS] = Default::default();
        for (id, player) in order {
            let wanted = usize::from(player.seat) % SEATS;
            let free = (0..SEATS)
                .map(|step| wanted.saturating_add(step).checked_rem(SEATS).unwrap_or(0))
                .find(|at| seats.get(*at).is_some_and(Option::is_none));
            if let Some(slot) = free.and_then(|at| seats.get_mut(at)) {
                *slot = Some(id.clone());
            }
        }
        seats
    }

    /// The seat `player` sits in.
    #[must_use]
    pub fn seat_of(&self, player: &str) -> Option<usize> {
        self.seating()
            .iter()
            .position(|seat| seat.as_deref() == Some(player))
    }

    /// The first free seat, for someone sitting down.
    #[must_use]
    pub fn free_seat(&self) -> Option<usize> {
        self.seating().iter().position(Option::is_none)
    }

    /// The player who deals the next hand: whoever has been at the table
    /// longest.
    #[must_use]
    pub fn dealer(&self) -> Option<&str> {
        self.players
            .iter()
            .min_by(|a, b| (a.1.joined_ms, a.0).cmp(&(b.1.joined_ms, b.0)))
            .map(|(id, _)| id.as_str())
    }

    /// Chips in front of `player`: their stack in the hand while they play
    /// one, else what they sat down with or won.
    #[must_use]
    pub fn chips(&self, player: &str) -> i64 {
        if self.hand.street.in_play()
            && let Some(p) = self.hand.players.iter().find(|p| p.player == player)
        {
            return p.stack;
        }
        self.players.get(player).map_or(0, |p| p.chips)
    }

    /// The players a deal now would deal in, in seat order.
    #[must_use]
    pub fn ready(&self) -> Vec<(usize, String)> {
        self.seating()
            .into_iter()
            .enumerate()
            .filter_map(|(seat, id)| id.map(|id| (seat, id)))
            .filter(|(_, id)| self.players.get(id).is_some_and(|p| p.chips > 0))
            .collect()
    }

    /// The earliest instant anyone but the dealer may deal, in milliseconds
    /// since the Unix epoch: the hand's end plus [`DEAL_GRACE_MS`]. `None`
    /// while a hand is in play, where nobody deals regardless.
    #[must_use]
    pub fn deal_opens_ms(&self) -> Option<i64> {
        (!self.hand.street.in_play()).then(|| self.hand.since_ms.saturating_add(DEAL_GRACE_MS))
    }

    /// The earliest instant anyone may fold for the player to act, in
    /// milliseconds since the Unix epoch: [`TURN_MS`] after their turn began.
    /// `None` when nobody is to act.
    #[must_use]
    pub fn skip_opens_ms(&self) -> Option<i64> {
        if !self.hand.street.in_play() {
            return None;
        }
        self.hand
            .to_act
            .map(|_| self.hand.since_ms.saturating_add(TURN_MS))
    }

    /// Whether `player` may deal now.
    #[must_use]
    pub fn may_deal(&self, player: &str, now_ms: i64) -> bool {
        !self.hand.street.in_play()
            && self.players.contains_key(player)
            && self.ready().len() >= 2
            && (self.dealer() == Some(player)
                || now_ms.saturating_sub(self.hand.since_ms) >= DEAL_GRACE_MS)
    }

    /// Write the hand's stacks back to the players still at the table.
    fn settle(&mut self) {
        for p in &self.hand.players {
            if let Some(player) = self.players.get_mut(&p.player) {
                player.chips = p.stack;
            }
        }
    }

    /// The position of the author in the hand, when it is their turn.
    fn acting(&self, cx: &ActionCx) -> Result<usize, &'static str> {
        let me = cx.author().ok_or("only a player can act")?;
        if !self.hand.street.in_play() {
            return Err("no hand is being played");
        }
        let at = self.hand.position(me).ok_or("you are not in this hand")?;
        if self.hand.to_act.and_then(|a| usize::try_from(a).ok()) != Some(at) {
            return Err("it is not your turn");
        }
        Ok(at)
    }

    fn after_move(&mut self, at: usize, now_ms: i64) {
        if let Some(p) = self.hand.players.get_mut(at) {
            p.acted = true;
        }
        self.hand.advance(at, now_ms);
        if self.hand.street == Street::Done {
            self.settle();
        }
    }

    /// Put the bet of the player at `at` up to `to` in total on this street.
    fn bet_to(&mut self, at: usize, to: i64) {
        let hand = &mut self.hand;
        let Some(p) = hand.players.get(at) else {
            return;
        };
        let before = hand.current_bet;
        let had_bet = before > 0;
        let added = hand.put_in(at, to.saturating_sub(p.bet));
        let Some(p) = hand.players.get_mut(at) else {
            return;
        };
        let total = p.bet;
        p.last = if p.stack == 0 {
            Move::AllIn
        } else if total <= before {
            if added == 0 { Move::Check } else { Move::Call }
        } else if had_bet {
            Move::Raise
        } else {
            Move::Bet
        };
        if total > before {
            let raise = total.saturating_sub(before);
            // A short all-in raise does not reopen the betting by its full
            // size, but everyone still has to answer it.
            if raise >= hand.min_raise {
                hand.min_raise = raise;
            }
            hand.current_bet = total;
            for (other, q) in hand.players.iter_mut().enumerate() {
                if other != at {
                    q.acted = false;
                }
            }
        }
    }
}

#[resource(kind = "so.tilo.poker.table", label = "Poker table")]
impl Table {
    /// Take a seat at the table, with a buy-in of chips.
    #[action]
    pub fn sit(
        &mut self,
        cx: &mut ActionCx,
        #[param("The seat to take, from 1 to 6; the next free one when it is taken")] seat: u32,
    ) -> Result<(), &'static str> {
        let me = cx
            .author()
            .ok_or("only a participant can sit down")?
            .to_owned();
        if self.players.contains_key(&me) {
            return Err("you already have a seat");
        }
        if self.free_seat().is_none() {
            return Err("every seat is taken");
        }
        let wanted = usize::try_from(seat.saturating_sub(1)).unwrap_or(0) % SEATS;
        let seat = if self.seating().get(wanted).is_some_and(Option::is_none) {
            wanted
        } else {
            self.free_seat().unwrap_or(0)
        };
        self.players.insert(
            me,
            Player {
                seat: u8::try_from(seat).unwrap_or(0),
                chips: BUY_IN,
                joined_ms: cx.now_ms(),
            },
        );
        Ok(())
    }

    /// Leave the table. A hand you are playing is folded.
    #[action]
    pub fn leave(&mut self, cx: &mut ActionCx) -> Result<(), &'static str> {
        let me = cx.author().ok_or("only a player can leave")?.to_owned();
        if !self.players.contains_key(&me) {
            return Err("you are not at the table");
        }
        if self.hand.is_live(&me)
            && let Some(at) = self.hand.position(&me)
        {
            let turn = self.hand.to_act.and_then(|a| usize::try_from(a).ok()) == Some(at);
            if let Some(p) = self.hand.players.get_mut(at) {
                p.folded = true;
                p.last = Move::Fold;
            }
            if turn || self.hand.live() <= 1 {
                self.after_move(at, cx.now_ms());
            }
        }
        self.players.remove(&me);
        Ok(())
    }

    /// Deal the next hand: shuffle, move the button and post the blinds.
    #[action]
    pub fn deal(&mut self, cx: &mut ActionCx) -> Result<(), &'static str> {
        let me = cx.author().ok_or("only a player can deal")?.to_owned();
        let now = cx.now_ms();
        if self.hand.street.in_play() {
            return Err("a hand is being played");
        }
        if !self.players.contains_key(&me) {
            return Err("sit down to deal");
        }
        let ready = self.ready();
        if ready.len() < 2 {
            return Err("two players with chips are needed");
        }
        if !self.may_deal(&me, now) {
            return Err("the dealer deals the next hand");
        }
        let uuid = cx.uuid();
        let seed = uuid
            .as_bytes()
            .iter()
            .take(8)
            .fold(0_u64, |seed, byte| (seed << 8) | u64::from(*byte));
        let mut deck = cards::shuffled(seed);
        // The button moves to the next player left of the last button.
        let last = usize::from(self.button_seat);
        let button = ready.iter().position(|(seat, _)| *seat > last).unwrap_or(0);
        let mut players: Vec<InHand> = ready
            .iter()
            .map(|(seat, id)| InHand {
                player: id.clone(),
                seat: u8::try_from(*seat).unwrap_or(0),
                stack: self.players.get(id).map_or(0, |p| p.chips),
                ..InHand::default()
            })
            .collect();
        for _ in 0..2 {
            for p in &mut players {
                p.cards.push(deck.pop().unwrap_or(0));
            }
        }
        self.button_seat = ready
            .get(button)
            .and_then(|(seat, _)| u8::try_from(*seat).ok())
            .unwrap_or(0);
        let n = players.len();
        let mut hand = Hand {
            number: self.hand.number.saturating_add(1),
            street: Street::Preflop,
            players,
            button: u32::try_from(button).unwrap_or(0),
            deck,
            board: Vec::new(),
            to_act: None,
            current_bet: BIG_BLIND,
            min_raise: BIG_BLIND,
            since_ms: now,
            awards: Vec::new(),
        };
        // Heads up, the button posts the small blind and acts first.
        let small = if n == 2 { button } else { after(button, n) };
        let big = after(small, n);
        hand.put_in(small, SMALL_BLIND);
        hand.put_in(big, BIG_BLIND);
        for (at, blind) in [(small, Move::SmallBlind), (big, Move::BigBlind)] {
            if let Some(p) = hand.players.get_mut(at) {
                p.last = if p.stack == 0 { Move::AllIn } else { blind };
            }
        }
        hand.current_bet = hand
            .players
            .iter()
            .map(|p| p.bet)
            .max()
            .unwrap_or(BIG_BLIND)
            .max(BIG_BLIND);
        let first = hand.next_from(big, InHand::can_act);
        hand.give_turn(first, now);
        // With everyone but one all in on the blinds, nobody bets.
        let current = hand.current_bet;
        let open = hand
            .players
            .iter()
            .filter(|p| p.can_act() && (!p.acted || p.bet < current))
            .count();
        if hand.players.iter().filter(|p| p.can_act()).count() < 2
            && hand.actor().is_none_or(|p| p.bet >= current)
            && open <= 1
        {
            hand.next_street(now);
        }
        self.hand = hand;
        if self.hand.street == Street::Done {
            self.settle();
        }
        Ok(())
    }

    /// Fold your hand.
    #[action]
    pub fn fold(&mut self, cx: &mut ActionCx) -> Result<(), &'static str> {
        let at = self.acting(cx)?;
        if let Some(p) = self.hand.players.get_mut(at) {
            p.folded = true;
            p.last = Move::Fold;
        }
        self.after_move(at, cx.now_ms());
        Ok(())
    }

    /// Check when nobody has bet, or call the bet you face.
    #[action]
    pub fn call(&mut self, cx: &mut ActionCx) -> Result<(), &'static str> {
        let at = self.acting(cx)?;
        self.bet_to(at, self.hand.current_bet);
        self.after_move(at, cx.now_ms());
        Ok(())
    }

    /// Bet or raise to a total for this street.
    #[action]
    pub fn raise(
        &mut self,
        cx: &mut ActionCx,
        #[param("Your total bet on this street after the raise")] to: f64,
    ) -> Result<(), &'static str> {
        let at = self.acting(cx)?;
        let me = cx.author().unwrap_or_default().to_owned();
        let (least, most) = self
            .hand
            .raise_range(&me)
            .ok_or("you cannot raise, only call or fold")?;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "chip counts are far below i64's range; the value is clamped next"
        )]
        let to = (to.round() as i64).clamp(least, most);
        self.bet_to(at, to);
        self.after_move(at, cx.now_ms());
        Ok(())
    }

    /// Put all your chips in.
    #[action]
    pub fn all_in(&mut self, cx: &mut ActionCx) -> Result<(), &'static str> {
        let at = self.acting(cx)?;
        let to = self
            .hand
            .players
            .get(at)
            .map_or(0, |p| p.bet.saturating_add(p.stack));
        self.bet_to(at, to);
        self.after_move(at, cx.now_ms());
        Ok(())
    }

    /// Show your cards to the table after the hand.
    #[action]
    pub fn show(&mut self, cx: &mut ActionCx) -> Result<(), &'static str> {
        let me = cx.author().ok_or("only a player can show cards")?;
        if self.hand.street.in_play() {
            return Err("show your cards once the hand is over");
        }
        let at = self
            .hand
            .position(me)
            .ok_or("you were not in the last hand")?;
        if let Some(p) = self.hand.players.get_mut(at) {
            p.shown = true;
        }
        Ok(())
    }

    /// Fold the hand of a player who has taken longer than 30 seconds.
    #[action]
    pub fn skip(&mut self, cx: &mut ActionCx) -> Result<(), &'static str> {
        let me = cx.author().ok_or("only a player can do that")?;
        if !self.players.contains_key(me) {
            return Err("sit down first");
        }
        let at = self
            .hand
            .to_act
            .and_then(|a| usize::try_from(a).ok())
            .filter(|_| self.hand.street.in_play())
            .ok_or("nobody is to act")?;
        if cx.now_ms().saturating_sub(self.hand.since_ms) < TURN_MS {
            return Err("they still have time");
        }
        if let Some(p) = self.hand.players.get_mut(at) {
            p.folded = true;
            p.last = Move::Fold;
        }
        self.after_move(at, cx.now_ms());
        Ok(())
    }

    /// Buy back in once you have no chips.
    #[action]
    pub fn rebuy(&mut self, cx: &mut ActionCx) -> Result<(), &'static str> {
        let me = cx.author().ok_or("only a player can rebuy")?.to_owned();
        if self.hand.is_live(&me) {
            return Err("wait for the hand to end");
        }
        let player = self.players.get_mut(&me).ok_or("sit down first")?;
        if player.chips > 0 {
            return Err("you still have chips");
        }
        player.chips = BUY_IN;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seated(ids: &[&str]) -> Table {
        let mut table = Table::default();
        for (seat, id) in ids.iter().enumerate() {
            table.players.insert(
                (*id).to_owned(),
                Player {
                    seat: u8::try_from(seat).unwrap(),
                    chips: BUY_IN,
                    joined_ms: i64::try_from(seat).unwrap(),
                },
            );
        }
        table
    }

    #[test]
    fn two_people_asking_for_one_seat_both_sit() {
        let mut table = Table::default();
        for (id, joined) in [("b", 2), ("a", 1)] {
            table.players.insert(
                id.to_owned(),
                Player {
                    seat: 3,
                    chips: BUY_IN,
                    joined_ms: joined,
                },
            );
        }
        let seating = table.seating();
        assert_eq!(seating[3].as_deref(), Some("a"));
        assert_eq!(seating[4].as_deref(), Some("b"));
    }

    #[test]
    fn side_pot_returns_an_uncalled_excess() {
        let mut hand = Hand {
            street: Street::River,
            board: vec![0, 14, 28, 42, 5],
            players: vec![
                InHand {
                    player: "a".into(),
                    committed: 100,
                    cards: vec![12, 25],
                    ..InHand::default()
                },
                InHand {
                    player: "b".into(),
                    committed: 500,
                    cards: vec![1, 2],
                    ..InHand::default()
                },
            ],
            ..Hand::default()
        };
        hand.finish(0);
        // a's pair of aces wins the 200 both could win; b takes back 400.
        assert_eq!(hand.players[0].stack, 200);
        assert_eq!(hand.players[1].stack, 400);
        let _ = seated(&[]);
    }
}

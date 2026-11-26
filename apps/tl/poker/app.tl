app so.tilo.poker "Poker" (
    icon: .trophy,
    category: .game,
    tagline: "Texas hold'em with everyone in the workspace",
    keywords: ["poker", "texas hold'em", "holdem", "cards", "card game", "game night", "chips"],
    splash_budget: 250,
)

/// Where a hand has got to.
enum Street {
    idle
    preflop
    flop
    turn
    river
    done
}

/// What a player did last in the hand.
enum Move {
    none
    small_blind
    big_blind
    check
    call
    bet
    raise
    all_in
    fold
}

/// What a five-card hand is, weakest first: the order its worth compares in.
enum Category {
    high_card
    pair
    two_pair
    trips
    straight
    flush
    full_house
    quads
    straight_flush
}

/// One player's part in the hand being played.
type InHand {
    player: Actor
    seat: Int
    stack: Int merge counter = 0
    /// Chips bet on this street.
    bet: Int merge counter = 0
    /// Chips put in over the whole hand.
    committed: Int merge counter = 0
    /// Their two cards: numbers below 52, the remainder by 13 a rank and the quotient a suit.
    cards: List<Int>
    folded: Bool = false
    /// Whether they have acted since the last bet or raise.
    acted: Bool = false
    /// Whether their cards are shown to everyone.
    shown: Bool = false
    last: Move = .none
}

/// What a player won when a hand ended.
type Award {
    player: Actor
    amount: Int
    /// What their hand was; none when everyone else folded.
    category: Option<Category> = .none
}

/// The hand being played, or the last one.
type Hand {
    /// Counts from 1; 0 before the first deal.
    number: Int merge counter = 0
    street: Street = .idle
    /// The players dealt in, in the order they were dealt.
    players: Collection<InHand>
    /// The place of the dealer button among them.
    button: Int = 0
    /// The cards not dealt yet, in the order they come off the deck.
    deck: List<Int>
    /// How many of the deck's cards are dealt: the next one is at this place.
    next: Int merge counter = 0
    /// The community cards.
    board: List<Int>
    /// The place of the player to act, while betting goes on.
    to_act: Option<Int> = .none
    /// The highest bet on this street.
    current_bet: Int = 0
    /// The smallest raise allowed: the size of the last bet or raise.
    min_raise: Int = 10
    /// When the player to act was given the turn, or when the hand ended.
    since: Option<Time> = .none
    awards: Collection<Award>
}

/// One person at the table, between hands.
type Player {
    person: Actor
    /// The seat they asked for.
    seat: Int
    /// Chips in front of them between hands.
    chips: Int
    /// When they sat down: the earlier of two people asking for one seat gets it.
    joined: Time
}

/// The table: who sits at it and the hand being played.
type Table (untitled: "Poker table") {
    /// What the window is called: the hand being played, once one is.
    title: Text = ""
    players: Collection<Player>
    hand: Hand
    /// The seat the button was on for the last hand.
    button_seat: Int = 0
}

root table: Table

default view poker_table for Table

// ---------------------------------------------------------------------------
// The rules of Texas hold'em: cards, hands and pots. A card is a number below 52: its
// remainder by 13 is the rank (0 is a two, 12 an ace) and its quotient the suit (0 spades,
// 1 hearts, 2 diamonds, 3 clubs).
// ---------------------------------------------------------------------------
/// How many people may sit at the table.
fn seat_count() -> Int {
    6
}

/// What a person sits down with.
fn buy_in() -> Int {
    1000
}

fn small_blind() -> Int {
    5
}

fn big_blind() -> Int {
    10
}

/// How long a player's turn lasts before anyone may fold it for them.
fn turn_time() -> Duration {
    30s
}

/// How long after a hand anyone may deal the next one, beside the dealer.
fn deal_grace() -> Duration {
    15s
}

/// Whether betting is going on.
fn in_play(s: Street) -> Bool {
    s == .preflop or s == .flop or s == .turn or s == .river
}

fn rank(card: Int) -> Int {
    card % 13
}

fn suit(card: Int) -> Int {
    card / 13
}

/// Whether a card is a heart or a diamond.
fn is_red(card: Int) -> Bool {
    suit(card) == 1 or suit(card) == 2
}

/// How a rank is printed on a card face.
fn rank_label(card: Int) -> Text {
    if rank(card) == 0 {
        "2"
    } else if rank(card) == 1 {
        "3"
    } else if rank(card) == 2 {
        "4"
    } else if rank(card) == 3 {
        "5"
    } else if rank(card) == 4 {
        "6"
    } else if rank(card) == 5 {
        "7"
    } else if rank(card) == 6 {
        "8"
    } else if rank(card) == 7 {
        "9"
    } else if rank(card) == 8 {
        "10"
    } else if rank(card) == 9 {
        "J"
    } else if rank(card) == 10 { "Q" } else if rank(card) == 11 { "K" } else { "A" }
}

/// A suit as its glyph.
fn suit_glyph(card: Int) -> Text {
    if suit(card) == 0 {
        "♠"
    } else if suit(card) == 1 { "♥" } else if suit(card) == 2 { "♦" } else { "♣" }
}

/// The word a rank reads as to a screen reader.
fn rank_word(card: Int) -> Text {
    if rank(card) == 0 {
        std.say("Two")
    } else if rank(card) == 1 {
        std.say("Three")
    } else if rank(card) == 2 {
        std.say("Four")
    } else if rank(card) == 3 {
        std.say("Five")
    } else if rank(card) == 4 {
        std.say("Six")
    } else if rank(card) == 5 {
        std.say("Seven")
    } else if rank(card) == 6 {
        std.say("Eight")
    } else if rank(card) == 7 {
        std.say("Nine")
    } else if rank(card) == 8 {
        std.say("Ten")
    } else if rank(card) == 9 {
        std.say("Jack")
    } else if rank(card) == 10 {
        std.say("Queen")
    } else if rank(card) == 11 { std.say("King") } else { std.say("Ace") }
}

/// The word a suit reads as to a screen reader.
fn suit_word(card: Int) -> Text {
    if suit(card) == 0 {
        std.say("spades")
    } else if suit(card) == 1 {
        std.say("hearts")
    } else if suit(card) == 2 { std.say("diamonds") } else { std.say("clubs") }
}

/// What a screen reader says for a card ("Queen of hearts").
fn card_name(card: Int) -> Text {
    std.say("{rank_word(card)} of {suit_word(card)}")
}

/// What a hand is called in a sentence.
fn category_words(c: Category) -> Text {
    if c == .high_card {
        std.say("a high card")
    } else if c == .pair {
        std.say("a pair")
    } else if c == .two_pair {
        std.say("two pair")
    } else if c == .trips {
        std.say("three of a kind")
    } else if c == .straight {
        std.say("a straight")
    } else if c == .flush {
        std.say("a flush")
    } else if c == .full_house {
        std.say("a full house")
    } else if c == .quads { std.say("four of a kind") } else { std.say("a straight flush") }
}

/// A fresh deck in order.
fn fresh_deck() -> List<Int> {
    [
        0,
        1,
        2,
        3,
        4,
        5,
        6,
        7,
        8,
        9,
        10,
        11,
        12,
        13,
        14,
        15,
        16,
        17,
        18,
        19,
        20,
        21,
        22,
        23,
        24,
        25,
        26,
        27,
        28,
        29,
        30,
        31,
        32,
        33,
        34,
        35,
        36,
        37,
        38,
        39,
        40,
        41,
        42,
        43,
        44,
        45,
        46,
        47,
        48,
        49,
        50,
        51,
    ]
}

/// How many of `cards` hold each rank, as a number that sorts by how many first and rank after.
fn groups(cards: List<Int>) -> List<Int> {
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
        .map(r -> cards.count(c -> rank(c) == r) * 16 + r)
        .filter(g -> g / 16 > 0)
        .sort_by(g -> g, desc)
}

fn count_of(g: Int) -> Int {
    g / 16
}

fn rank_of(g: Int) -> Int {
    g % 16
}

fn at_or(xs: List<Int>, at: Int) -> Int {
    xs.at(at) ?? 0
}

fn has_rank(cards: List<Int>, r: Int) -> Bool {
    cards.any(c -> rank(c) == r)
}

/// The highest card of a straight among these five, the wheel counting as five high.
fn straight_high(cards: List<Int>) -> Option<Int> {
    let high = [12, 11, 10, 9, 8, 7, 6, 5, 4]
        .find(h -> [0, 1, 2, 3, 4].all(s -> has_rank(cards, h - s)))
    if high != .none {
        high
    } else if [12, 0, 1, 2, 3].all(r -> has_rank(cards, r)) { .some(3) } else { .none }
}

fn is_flush(cards: List<Int>) -> Bool {
    cards.all(c -> suit(c) == suit(at_or(cards, 0)))
}

/// What a five-card hand is.
fn category(cards: List<Int>) -> Category {
    let gs = groups(cards)
    if straight_high(cards) != .none and is_flush(cards) {
        .straight_flush
    } else if count_of(at_or(gs, 0)) == 4 {
        .quads
    } else if count_of(at_or(gs, 0)) == 3 and count_of(at_or(gs, 1)) == 2 {
        .full_house
    } else if is_flush(cards) {
        .flush
    } else if straight_high(cards) != .none {
        .straight
    } else if count_of(at_or(gs, 0)) == 3 {
        .trips
    } else if count_of(at_or(gs, 0)) == 2 and count_of(at_or(gs, 1)) == 2 {
        .two_pair
    } else if count_of(at_or(gs, 0)) == 2 { .pair } else { .high_card }
}

fn category_index(c: Category) -> Int {
    if c == .high_card {
        0
    } else if c == .pair {
        1
    } else if c == .two_pair {
        2
    } else if c == .trips {
        3
    } else if c == .straight {
        4
    } else if c == .flush { 5 } else if c == .full_house { 6 } else if c == .quads { 7 } else { 8 }
}

fn category_at(index: Int) -> Category {
    if index == 0 {
        .high_card
    } else if index == 1 {
        .pair
    } else if index == 2 {
        .two_pair
    } else if index == 3 {
        .trips
    } else if index == 4 {
        .straight
    } else if index == 5 {
        .flush
    } else if index == 6 { .full_house } else if index == 7 { .quads } else { .straight_flush }
}

/// What a five-card hand is worth to compare: higher wins, equal splits the pot.
fn five(cards: List<Int>) -> Int {
    let cat = category(cards)
    let raw = if cat == .straight or cat == .straight_flush {
        [straight_high(cards) ?? 0]
    } else { groups(cards).map(g -> rank_of(g)) }
    category_index(cat) * 371293
        + at_or(raw, 0) * 28561
        + at_or(raw, 1) * 2197
        + at_or(raw, 2) * 169
        + at_or(raw, 3) * 13
        + at_or(raw, 4)
}

/// The best five-card hand among `cards`, leaving out `left` more cards from `from` on.
fn pick(cards: List<Int>, from: Int, left: Int, held: List<Int>, best: Option<Int>) -> Option<Int> {
    if left == 0 {
        let score = five(held)
        if best == .none or score > (best ?? 0) { .some(score) } else { best }
    } else if from >= cards.len() {
        best
    } else {
        let card = cards.at(from) ?? 0
        let with = pick(cards, from + 1, left - 1, [held, [card]].flat_map(x -> x), best)
        pick(cards, from + 1, left, held, with)
    }
}

/// The best five-card hand among five to seven cards.
fn best(cards: List<Int>) -> Option<Int> {
    if cards.len() < 5 {
        return .none
    }
    pick(cards, 0, 5, [], .none)
}

/// How many of the players in the hand still hold cards.
fn live_count(t: Table) -> Int {
    t.hand.players.count(p -> not p.folded)
}

/// Whether a player can still bet: in the hand with chips behind.
fn can_act(p: InHand) -> Bool {
    not p.folded and p.stack > 0
}

/// How many players can still act.
fn actors(t: Table) -> Int {
    t.hand.players.count(p -> can_act(p))
}

/// Whether a player may still act on this street: they have not acted, or they face a bet.
fn facing(p: InHand, current: Int) -> Bool {
    can_act(p) and (not p.acted or p.bet < current)
}

/// How many players may still act on this street.
fn open_count(t: Table) -> Int {
    t.hand.players.count(p -> facing(p, t.hand.current_bet))
}

/// The place of the player after `from` who may act, `step` places round.
fn next_from(t: Table, from: Int, step: Int) -> Option<Int> {
    let n = t.hand.players.len()
    if n == 0 or step > n {
        return .none
    }
    let at = (from + step) % n
    if let p = t.hand.players.at(at) {
        if facing(p, t.hand.current_bet) {
            return .some(at)
        }
    }
    next_from(t, from, step + 1)
}

/// Who sits in each seat, from seat 0 to 5: their person's id, empty for a free seat.
fn seating(t: Table) -> List<Text> {
    let taken = seat_map(t)
    [0, 1, 2, 3, 4, 5].map(s -> taken.get(s.to_text()) ?? "")
}

/// Who sits where, by seat number as text.
fn seat_map(t: Table) -> Map<Text> {
    place_all(t.players.sort_by(p -> p.joined.iso()), Map<Text>())
}

/// Seats each player of `order` in turn: the seat they asked for, else the next free one.
fn place_all(order: List<Player>, taken: Map<Text>) -> Map<Text> {
    if let p = order.first() {
        let seat = free_from(p.seat % seat_count(), taken, 0)
        taken.put(seat.to_text(), p.person.id())
        place_all(rest_players(order, 1), taken)
    } else { taken }
}

/// The first free seat from `start` on.
fn free_from(start: Int, taken: Map<Text>, step: Int) -> Int {
    let seat = (start + step) % seat_count()
    if taken.get(seat.to_text()) == .none { seat } else { free_from(start, taken, step + 1) }
}

/// The first free seat, for someone sitting down.
fn free_seat(t: Table) -> Option<Int> {
    seating(t).index_of("")
}

/// The player with the person id `id`.
fn player_of(t: Table, id: Text) -> Option<Player> {
    t.players.find(p -> p.person.id() == id)
}

/// The player who deals the next hand: whoever has been at the table longest.
fn dealer(t: Table) -> Option<Actor> {
    if let p = t.players.sort_by(p -> p.joined.iso()).first() { .some(p.person) } else { .none }
}

/// The players a deal now would deal in, in seat order.
fn ready(t: Table) -> List<Player> {
    let places = seating(t)
    t.players.filter(p -> p.chips > 0).sort_by(p -> places.index_of(p.person.id()) ?? seat_count())
}

/// The place of `who` in the hand being played.
fn position(t: Table, who: Actor) -> Option<Int> {
    t.hand.players.map(p -> p.player).index_of(who)
}

/// What `who` needs to put in to call.
fn to_call(t: Table, who: Actor) -> Int {
    if let at = position(t, who) {
        if let p = t.hand.players.at(at) {
            return (t.hand.current_bet - p.bet).min(p.stack).max(0)
        }
    }
    0
}

/// Chips in front of `who`: their stack while they play a hand, else what they sat down with or
/// won.
fn chips(t: Table, who: Actor) -> Int {
    if in_play(t.hand.street) {
        if let at = position(t, who) {
            if let p = t.hand.players.at(at) {
                return p.stack
            }
        }
    }
    if let p = t.players.find(p -> p.person == who) { p.chips } else { 0 }
}

/// The smallest total bet a player may raise to.
fn raise_least(t: Table, who: Actor) -> Int {
    (t.hand.current_bet + t.hand.min_raise.max(big_blind())).min(raise_most(t, who))
}

/// The largest total bet a player may raise to.
fn raise_most(t: Table, who: Actor) -> Int {
    if let at = position(t, who) {
        if let p = t.hand.players.at(at) {
            return p.bet + p.stack
        }
    }
    0
}

/// Whether a player may bet or raise rather than only call or fold.
fn can_raise(t: Table, who: Actor) -> Bool {
    raise_most(t, who) > t.hand.current_bet
}

/// Whether the player whose turn it is has acted since the last bet or raise.
fn has_acted(t: Table) -> Bool {
    if let at = t.hand.to_act {
        if let p = t.hand.players.at(at) { p.acted } else { true }
    } else { true }
}

/// The player to act.
fn actor(t: Table) -> Option<InHand> {
    if let at = t.hand.to_act { t.hand.players.at(at) } else { .none }
}

/// What the player at `at` holds, worth comparing.
fn score_at(t: Table, at: Int) -> Int {
    if let p = t.hand.players.at(at) {
        return best([p.cards, t.hand.board].flat_map(x -> x)) ?? -1
    }
    -1
}

/// The best hand among the players at `places`, or -1 when none is known.
fn best_among(t: Table, places: List<Int>) -> Int {
    let best = -1
    for at in places {
        let score = score_at(t, at)
        if score > best {
            best = score
        }
    }
    best
}

/// The numbers of `xs` from `at` on.
fn rest_ints(xs: List<Int>, at: Int) -> List<Int> {
    if at >= xs.len() {
        []
    } else if let one = xs.at(at) {
        [[one], rest_ints(xs, at + 1)].flat_map(x -> x)
    } else { [] }
}

/// The players of `xs` from `at` on.
fn rest_players(xs: List<Player>, at: Int) -> List<Player> {
    if at >= xs.len() {
        []
    } else if let one = xs.at(at) {
        [[one], rest_players(xs, at + 1)].flat_map(x -> x)
    } else { [] }
}

/// The numbers from 0 up to `n` less one.
fn indices(n: Int) -> List<Int> {
    if n <= 0 { [] } else { indices(n - 1) + [n - 1] }
}

fn total(xs: List<Int>) -> Int {
    if let x = xs.first() { x + total(rest_ints(xs, 1)) } else { 0 }
}

/// The chips each player put in that make a pot of their own: every all-in level, smallest first.
fn levels_of(committed: List<Int>, folded: List<Bool>) -> List<Int> {
    let values = indices(committed.len())
        .filter(at -> (committed.at(at) ?? 0) > 0 and not (folded.at(at) ?? false))
        .map(at -> committed.at(at) ?? 0)
        .sort_by(v -> v)
    dedupe(values, [])
}

fn dedupe(xs: List<Int>, kept: List<Int>) -> List<Int> {
    if let x = xs.first() {
        if kept.contains(x) {
            dedupe(rest_ints(xs, 1), kept)
        } else {
            dedupe(rest_ints(xs, 1), [kept, [x]].flat_map(k -> k))
        }
    } else { kept }
}

fn pot_amount(committed: List<Int>, level: Int, floor: Int) -> Int {
    total(committed.map(c -> (c.min(level) - floor).max(0)))
}

fn eligible_of(committed: List<Int>, folded: List<Bool>, level: Int) -> List<Int> {
    indices(committed.len())
        .filter(at -> (committed.at(at) ?? 0) >= level and not (folded.at(at) ?? false))
}

/// The winners in the order the odd chips go: from the player after the button.
fn winners_after_button(t: Table, winners: List<Int>) -> List<Int> {
    let n = t.hand.players.len()
    winners.sort_by(at -> (at + n - t.hand.button - 1) % n)
}

/// What a move reads as.
fn move_words(m: Move) -> Text {
    if m == .small_blind {
        std.say("Small blind")
    } else if m == .big_blind {
        std.say("Big blind")
    } else if m == .check {
        std.say("Check")
    } else if m == .call {
        std.say("Call")
    } else if m == .bet {
        std.say("Bet")
    } else if m == .raise {
        std.say("Raise")
    } else if m == .all_in {
        std.say("All in")
    } else if m == .fold { std.say("Folded") } else { "" }
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------
/// Takes a seat at the table, with a buy-in of chips.
primary action sit(
    t: Table,
    seat: Int (label: "Seat, from 1 to 6; the next free one when taken") = 0,
) uses time (icon: .plus, label: "Take a seat", did: "sat down") {
    require not t.players.any(p -> p.person == invoker) "You already have a seat"
    require free_seat(t) != .none "Every seat is taken"
    let wanted = if seat > 0 { seat - 1 } else { free_seat(t) ?? 0 } % seat_count()
    let chosen = if seating(t).at(wanted) == .some("") {
        wanted
    } else { free_from(wanted, seat_map(t), 0) }
    t.players.insert(Player(person: invoker, seat: chosen, chips: buy_in(), joined: now))
}

/// Leaves the table.
action leave(t: Table) uses time (
    icon: .arrow_left,
    label: "Leave the table",
    did: "left the table",
) {
    require t.players.any(p -> p.person == invoker) "You are not at the table"
    if in_play(t.hand.street) {
        if let at = position(t, invoker) {
            let was_turn = t.hand.to_act == .some(at)
            if let p = t.hand.players.at(at) {
                p.folded = true
                p.acted = true
                p.last = .fold
            }
            if was_turn or live_count(t) <= 1 { advance(t) }
        }
    }
    if let p = t.players.find(p -> p.person == invoker) { t.players.remove(p) }
}

/// Deals the next hand: shuffles, moves the button and posts the blinds.
action deal(
    t: Table,
    at: Time (label: "The clock the deal's grace is judged by") = Time("1970-01-01T00:00:00Z"),
) uses random, time (icon: .play, label: "Deal next hand", did: "dealt a hand") {
    require not in_play(t.hand.street) "A hand is being played"
    require t.players.any(p -> p.person == invoker) "Sit down to deal"
    require ready(t).len() >= 2 "Two players with chips are needed"
    require dealer(t) == .some(invoker) or (t.hand.since ?? at) + deal_grace() <= at "The dealer deals the next hand"
    // The clock the caller gave, or this invocation's own when it gave none (the epoch
    // stands for none, so an offer pressed without one is judged by the clock).
    let moment = if at > Time("1970-01-01T00:00:00Z") { at } else { now }
    let places = seating(t)
    let seat_list = ready(t).map(p -> places.index_of(p.person.id()) ?? 0)
    let after = seat_list.find(s -> s > t.button_seat)
    let button = if after == .none { 0 } else { seat_list.index_of(after ?? 0) ?? 0 }
    t.button_seat = at_or(seat_list, button)
    t.title = std.say("Hand {t.hand.number + 1}")
    // The new hand starts fresh: the last one's players and awards go, and its fields reset.
    for hp in t.hand.players.map(p -> p) {
        t.hand.players.remove(hp)
    }
    t.hand.awards.remove_where(a -> true)
    t.hand.number += 1
    t.hand.street = .preflop
    t.hand.button = button
    t.hand.deck = random.shuffle(fresh_deck())
    t.hand.next -= t.hand.next
    t.hand.board = []
    t.hand.current_bet = big_blind()
    t.hand.min_raise = big_blind()
    t.hand.to_act = .none
    t.hand.since = .some(moment)
    for p in ready(t) {
        t.hand.players.insert(
            InHand(player: p.person, seat: places.index_of(p.person.id()) ?? 0, stack: p.chips),
        )
    }
    let drawn = 0
    for round in [1, 2] {
        for p in t.hand.players {
            if let card = t.hand.deck.at(drawn) {
                p.cards = [p.cards, [card]].flat_map(x -> x)
            }
            drawn = drawn + 1
        }
    }
    t.hand.next += drawn
    let n = t.hand.players.len()
    let small = if n == 2 { button } else { (button + 1) % n }
    let big = (small + 1) % n
    if let p = t.hand.players.at(small) {
        p.stack -= small_blind()
        p.bet += small_blind()
        p.committed += small_blind()
        p.last = if p.stack == 0 { .all_in } else { .small_blind }
    }
    if let p = t.hand.players.at(big) {
        p.stack -= big_blind()
        p.bet += big_blind()
        p.committed += big_blind()
        p.last = if p.stack == 0 { .all_in } else { .big_blind }
    }
    let highest = 0
    for p in t.hand.players {
        highest = highest.max(p.bet)
    }
    t.hand.current_bet = highest.max(big_blind())
    t.hand.to_act = next_from(t, big, 1)
    t.hand.since = .some(moment)
    if open_count(t) == 0 { advance(t) }
}

/// Moves the hand on once its street's betting is done: the next player's turn, the next cards
/// dealt, or the hand paid out. Every move calls it; a call of its own changes only what the
/// table's rules have already decided, and refuses while a move is open.
action advance(t: Table) uses time (icon: .chevron_right, label: "Move the hand on") {
    require in_play(t.hand.street) "No hand is being played"
    require has_acted(t) or open_count(t) == 0 or actors(t) < 2 "A move is still open"
    if live_count(t) <= 1 {
        settle_hand(t)
        return
    }
    let next = if let at = t.hand.to_act { next_from(t, at, 1) } else { .none }
    if next != .none {
        t.hand.to_act = next
        t.hand.since = .some(now)
        return
    }
    // The betting closed: the next cards come, one burnt first, unless the board is out.
    if t.hand.street != .river {
        close_bets(t)
        if t.hand.street == .preflop {
            t.hand.street = .flop
            t.hand.next += 1
            t.hand.board = [t.hand.board, rest_ints(t.hand.deck, t.hand.next).take(3)]
                .flat_map(x -> x)
            t.hand.next += 3
        } else if t.hand.street == .flop {
            t.hand.street = .turn
            t.hand.next += 1
            t.hand.board = [t.hand.board, rest_ints(t.hand.deck, t.hand.next).take(1)]
                .flat_map(x -> x)
            t.hand.next += 1
        } else if t.hand.street == .turn {
            t.hand.street = .river
            t.hand.next += 1
            t.hand.board = [t.hand.board, rest_ints(t.hand.deck, t.hand.next).take(1)]
                .flat_map(x -> x)
            t.hand.next += 1
        }
        if actors(t) >= 2 {
            t.hand.to_act = next_from(t, t.hand.button, 1)
            t.hand.since = .some(now)
            return
        }
    }
    if t.hand.street != .river {
        close_bets(t)
        if t.hand.street == .flop {
            t.hand.street = .turn
            t.hand.next += 1
            t.hand.board = [t.hand.board, rest_ints(t.hand.deck, t.hand.next).take(1)]
                .flat_map(x -> x)
            t.hand.next += 1
        } else if t.hand.street == .turn {
            t.hand.street = .river
            t.hand.next += 1
            t.hand.board = [t.hand.board, rest_ints(t.hand.deck, t.hand.next).take(1)]
                .flat_map(x -> x)
            t.hand.next += 1
        }
        if actors(t) >= 2 {
            t.hand.to_act = next_from(t, t.hand.button, 1)
            t.hand.since = .some(now)
            return
        }
    }
    if t.hand.street == .turn {
        close_bets(t)
        t.hand.street = .river
        t.hand.next += 1
        t.hand.board = [t.hand.board, rest_ints(t.hand.deck, t.hand.next).take(1)].flat_map(x -> x)
        t.hand.next += 1
        if actors(t) >= 2 {
            t.hand.to_act = next_from(t, t.hand.button, 1)
            t.hand.since = .some(now)
            return
        }
    }
    settle_hand(t)
}

/// Puts every player's bet back to zero for the next street: what closing a street's betting
/// leaves behind.
action close_bets(t: Table) {
    require in_play(t.hand.street) "No hand is being played"
    require actors(t) < 2 or open_count(t) == 0 "The betting is still open"
    for p in t.hand.players {
        p.bet -= p.bet
        p.acted = false
        if p.last != .fold and p.last != .all_in {
            p.last = .none
        }
    }
    t.hand.current_bet = 0
    t.hand.min_raise = big_blind()
}

/// Pays the pots and ends the hand. The rules of the table call it once the hand is decided; a
/// call of its own pays what the showdown pays anyway and refuses while betting is open.
action settle_hand(t: Table) uses time {
    require in_play(t.hand.street) "No hand is being played"
    require live_count(t) <= 1 or actors(t) < 2 or open_count(t) == 0 "The hand is still being played"
    let committed = t.hand.players.map(p -> p.committed)
    let folded = t.hand.players.map(p -> p.folded)
    let contested = live_count(t) > 1
    let levels = levels_of(committed, folded)
    let counted = 0
    let floor = 0
    for level in levels {
        counted = counted + pot_amount(committed, level, floor)
        floor = level
    }
    let won = Map<Int>()
    let floor = 0
    for level in levels {
        let extra = if level == (levels.last() ?? 0) { total(committed) - counted } else { 0 }
        let amount = pot_amount(committed, level, floor) + extra
        let eligible = eligible_of(committed, folded, level)
        if amount > 0 and not eligible.is_empty() {
            let best = if contested { best_among(t, eligible) } else { -1 }
            let winners = if contested {
                eligible.filter(at -> score_at(t, at) == best)
            } else { eligible }
            let count = winners.len()
            if count > 0 {
                let share = amount / count
                let odd = amount % count
                let place = 0
                for at in winners_after_button(t, winners) {
                    let plus = if place < odd { 1 } else { 0 }
                    won.put(at.to_text(), (won.get(at.to_text()) ?? 0) + share + plus)
                    place = place + 1
                }
            }
        }
        floor = level
    }
    t.hand.awards.remove_where(a -> true)
    for at in indices(t.hand.players.len()) {
        if let p = t.hand.players.at(at) {
            let amount = won.get(at.to_text()) ?? 0
            p.stack += amount
            p.bet -= p.bet
            if contested and not p.folded {
                p.shown = true
            }
            if amount > 0 {
                let score = score_at(t, at)
                let hand: Option<Category> = if contested and score >= 0 {
                    .some(category_at(score / 371293))
                } else { .none }
                t.hand.awards.insert(Award(player: p.player, amount: amount, category: hand))
            }
        }
    }
    // What each player has after the hand is what they bring to the next one.
    for at in indices(t.hand.players.len()) {
        if let hp = t.hand.players.at(at) {
            if let player = t.players.find(p -> p.person == hp.player) {
                player.chips = hp.stack
            }
        }
    }
    t.hand.street = .done
    t.hand.to_act = .none
    t.hand.since = .some(now)
}

/// Folds your hand.
action fold(t: Table) uses time (icon: .x, label: "Fold", did: "folded") {
    require in_play(t.hand.street) "No hand is being played"
    require position(t, invoker) != .none "You are not in this hand"
    require t.hand.to_act == position(t, invoker) "It is not your turn"
    let at = position(t, invoker) ?? 0
    if let p = t.hand.players.at(at) {
        p.folded = true
        p.last = .fold
        p.acted = true
    }
    advance(t)
}

/// Checks when nobody has bet, or calls the bet you face.
action call(t: Table) uses time (icon: .check, label: "Check or call", did: "checked or called") {
    require in_play(t.hand.street) "No hand is being played"
    require position(t, invoker) != .none "You are not in this hand"
    require t.hand.to_act == position(t, invoker) "It is not your turn"
    let at = position(t, invoker) ?? 0
    let before = t.hand.current_bet
    if let p = t.hand.players.at(at) {
        let added = (before - p.bet).min(p.stack).max(0)
        p.stack -= added
        p.bet += added
        p.committed += added
        p.last = if p.stack == 0 { .all_in } else if added == 0 { .check } else { .call }
        p.acted = true
    }
    advance(t)
}

/// Bets or raises to a total for this street.
action raise(
    t: Table,
    to: Float (label: "Your total bet on this street after the raise"),
) uses time (icon: .trending_up, label: "Bet or raise", did: "raised") {
    require in_play(t.hand.street) "No hand is being played"
    require position(t, invoker) != .none "You are not in this hand"
    require t.hand.to_act == position(t, invoker) "It is not your turn"
    require can_raise(t, invoker) "You cannot raise, only call or fold"
    let at = position(t, invoker) ?? 0
    let total = to.round().clamp(raise_least(t, invoker), raise_most(t, invoker))
    let before = t.hand.current_bet
    let had_bet = before > 0
    if let p = t.hand.players.at(at) {
        let added = (total - p.bet).min(p.stack).max(0)
        p.stack -= added
        p.bet += added
        p.committed += added
        let paid = p.bet
        p.last = if p.stack == 0 {
            .all_in
        } else if paid > before {
            if had_bet { .raise } else { .bet }
        } else if added == 0 { .check } else { .call }
        if paid > before {
            let grown = paid - before
            if grown >= t.hand.min_raise {
                t.hand.min_raise = grown
            }
            t.hand.current_bet = paid
            for q in t.hand.players {
                if q != p {
                    q.acted = false
                }
            }
        }
        p.acted = true
    }
    advance(t)
}

/// Puts all your chips in.
action all_in(t: Table) uses time (icon: .flame, label: "All in", did: "went all in") {
    require in_play(t.hand.street) "No hand is being played"
    require position(t, invoker) != .none "You are not in this hand"
    require t.hand.to_act == position(t, invoker) "It is not your turn"
    let at = position(t, invoker) ?? 0
    let before = t.hand.current_bet
    let had_bet = before > 0
    if let p = t.hand.players.at(at) {
        let added = p.stack
        p.stack -= added
        p.bet += added
        p.committed += added
        let paid = p.bet
        p.last = .all_in
        if paid > before {
            let grown = paid - before
            if grown >= t.hand.min_raise {
                t.hand.min_raise = grown
            }
            t.hand.current_bet = paid
            for q in t.hand.players {
                if q != p {
                    q.acted = false
                }
            }
        }
        p.acted = true
    }
    advance(t)
}

/// Shows your cards to the table after the hand.
action show(t: Table) (icon: .eye, label: "Show my cards", did: "showed their cards") {
    require not in_play(t.hand.street) "Show your cards once the hand is over"
    require position(t, invoker) != .none "You were not in the last hand"
    if let p = t.hand.players.at(position(t, invoker) ?? 0) {
        p.shown = true
    }
}

/// Folds the hand of a player who has taken longer than their turn.
action skip(
    t: Table,
    at: Time (label: "The clock the turn's grace is judged by") = Time("1970-01-01T00:00:00Z"),
) uses time (icon: .timer, label: "Fold for the slow player", did: "folded a slow player's hand") {
    require t.players.any(p -> p.person == invoker) "Sit down first"
    require in_play(t.hand.street) and t.hand.to_act != .none "Nobody is to act"
    require at >= (t.hand.since ?? at) + turn_time() "They still have time"
    if let p = t.hand.players.at(t.hand.to_act ?? 0) {
        p.folded = true
        p.last = .fold
        p.acted = true
    }
    advance(t)
}

/// Buys back in once you have no chips.
action rebuy(t: Table) (icon: .refresh, label: "Buy back in", did: "bought back in") {
    require t.players.any(p -> p.person == invoker) "Sit down first"
    require not (in_play(t.hand.street)
        and t.hand.players.any(p -> p.player == invoker and not p.folded)) "Wait for the hand to end"
    require 0 == (t.players.find(p -> p.person == invoker)?.chips ?? -1) "You still have chips"
    if let player = t.players.find(p -> p.person == invoker) {
        player.chips = buy_in()
    }
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------
/// The table as each person sees it: the seats round the board, their own cards face up and
/// everyone else's face down until the showdown, and the moves open to them now.
view poker_table(t: Table) {
    state raise_to: Float = 10.0
    std.page(t) {
        stack(gap: .group) {
            seat_row(t, [0, 1, 2])
            middle(t)
            seat_row(t, [5, 4, 3])
            status_lines(t)
            controls(t, raise_to)
        }
    }
}

/// Three seats side by side, centred.
view seat_row(t: Table, places: List<Int>) {
    row(gap: .sibling, distribute: .center, align: .start) {
        for place in places {
            seat_view(t, place)
        }
    }
}

/// One seat: who sits there, what they have, their cards and what they did last.
view seat_view(t: Table, place: Int) {
    let who = seating(t).at(place) ?? ""
    if who == "" {
        stack(gap: .sibling) {
            ui.TextStack("Seat {place + 1}", secondary: "Open")
            if not t.players.any(p -> p.person == invoker) {
                ui.Button("Sit here", role: .primary) {
                    on press: sit(t, place + 1)
                }
            }
        }
    } else if let p = player_of(t, who) {
        ui.Card(item: p, selected: p.person == invoker) {
            row(gap: .related, align: .center) {
                ui.Avatar(who: .some(p.person))
                ui.TextStack(
                    if p.person == invoker { "You" } else { p.person.name() },
                    secondary: "{chips(t, p.person)} chips",
                    tertiary: seat_state(t, p.person, invoker),
                )
            }
            cards_of(t, p.person)
        }
    }
}

/// What the seat says about the player's part in the hand, if anything.
fn seat_state(t: Table, who: Actor, viewer: Actor) -> Option<Text> {
    let at = position(t, who)
    if t.hand.street == .done {
        if let a = t.hand.awards.find(a -> a.player == who) {
            return .some(std.say("Won {a.amount}"))
        }
        if let p = t.hand.players.find(p -> p.player == who) {
            if p.folded {
                return .some(std.say("Folded"))
            }
        }
        return .none
    }
    if in_play(t.hand.street) and at != .none and t.hand.to_act == at {
        return .some(if who == viewer { std.say("Your turn") } else { std.say("Their turn") })
    }
    if let p = t.hand.players.find(p -> p.player == who) {
        if p.last != .none {
            if p.bet > 0 and not p.folded {
                return .some(std.say("{move_words(p.last)} {p.bet}"))
            }
            return .some(move_words(p.last))
        }
    }
    if t.hand.button == (at ?? -1) {
        return .some(std.say("Dealer"))
    }
    return .none
}

/// A player's two cards: face up for you and at the showdown, face down otherwise.
view cards_of(t: Table, who: Actor) {
    let at = position(t, who)
    row(gap: .tight, align: .center) {
        if at != .none {
            if let p = t.hand.players.at(at ?? 0) {
                for card in p.cards {
                    if who == invoker or p.shown {
                        card_face(card, false)
                    } else if not (p.folded and in_play(t.hand.street)) {
                        card_back(false)
                    }
                }
            }
        }
    }
}

/// A card's face, drawn on the theme.
view card_face(card: Int, big: Bool) {
    ui.Drawing(
        [
            ui.Shape.rect(
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
                round: 0.1,
                fill: .some(.base),
                stroke: .some(.track),
                weight: .hairline,
            ),
            ui.Shape.text(
                x: 0.12,
                y: 0.34,
                text: rank_label(card),
                role: if big { .heading } else { .label },
                ink: if is_red(card) { .danger } else { .ink },
            ),
            ui.Shape.text(
                x: 0.12,
                y: 0.62,
                text: suit_glyph(card),
                role: .caption,
                ink: if is_red(card) { .danger } else { .ink },
            ),
        ],
        meaning: .picture,
        label: card_name(card),
        width: if big { 56.0 } else { 40.0 },
        height: if big { 78.0 } else { 56.0 },
    )
}

/// A card face down.
view card_back(big: Bool) {
    ui.Drawing(
        [
            ui.Shape.rect(
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
                round: 0.1,
                fill: .some(.track),
                stroke: .some(.muted),
                weight: .hairline,
            ),
            ui.Shape.rect(x: 0.16, y: 0.12, w: 0.68, h: 0.76, round: 0.08, fill: .some(.surface)),
        ],
        meaning: .decoration,
        label: "Face-down card",
        width: if big { 56.0 } else { 40.0 },
        height: if big { 78.0 } else { 56.0 },
    )
}

/// Where no card has come yet.
view card_slot(big: Bool) {
    ui.Drawing(
        [
            ui.Shape.rect(
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
                round: 0.1,
                stroke: .some(.track),
                weight: .hairline,
            ),
        ],
        meaning: .decoration,
        label: "No card yet",
        width: if big { 56.0 } else { 40.0 },
        height: if big { 78.0 } else { 56.0 },
    )
}

/// The board, the pot and where the hand is.
view middle(t: Table) {
    stack(gap: .related) {
        row(gap: .related, distribute: .center) {
            for at in [0, 1, 2, 3, 4] {
                if let card = t.hand.board.at(at) {
                    card_face(card, true)
                } else {
                    card_slot(true)
                }
            }
        }
        row(gap: .sibling, distribute: .center, align: .center) {
            ui.Text(street_words(t.hand.street), role: .label)
            ui.Text("Pot {pot(t)}", role: .heading, numeric: true)
            ui.Text("Blinds {small_blind()} and {big_blind()}", role: .caption, secondary: true)
        }
    }
}

fn street_words(s: Street) -> Text {
    if s == .preflop {
        std.say("Preflop")
    } else if s == .flop {
        std.say("Flop")
    } else if s == .turn {
        std.say("Turn")
    } else if s == .river {
        std.say("River")
    } else if s == .done { std.say("Hand over") } else { std.say("Waiting to deal") }
}

/// Chips in the middle: everything put in, this street's bets included.
fn pot(t: Table) -> Int {
    total(t.hand.players.map(p -> p.committed))
}

/// One line on where the hand is, read out as it changes.
view status_lines(t: Table) {
    stack(gap: .related) {
        if t.hand.street == .done {
            for a in t.hand.awards {
                if let c = a.category {
                    ui.Text(
                        "{a.player.name()} wins {a.amount} with {category_words(c)}",
                        live: .polite,
                    )
                } else {
                    ui.Text("{a.player.name()} wins {a.amount}", live: .polite)
                }
            }
        } else if in_play(t.hand.street) {
            if let a = actor(t) {
                if a.player == invoker {
                    ui.Text(
                        if to_call(t, invoker) > 0 {
                            "Your turn: {to_call(t, invoker)} to call"
                        } else { "Your turn" },
                        live: .polite,
                    )
                } else {
                    ui.Text("Waiting for {a.player.name()}", live: .polite)
                }
            }
        }
        ui.Text(waiting_words(t, invoker), secondary: true)
    }
}

/// What the table waits for between hands.
fn waiting_words(t: Table, who: Actor) -> Text {
    if ready(t).len() < 2 {
        return std.say("Waiting for a second player to sit down")
    }
    if in_play(t.hand.street) {
        return ""
    }
    if dealer(t) == .some(who) {
        std.say("Deal the next hand when everyone is ready")
    } else if let d = dealer(t) {
        std.say("Waiting for {d.name()} to deal")
    } else {
        std.say("Waiting for a second player to sit down")
    }
}

/// The moves open to this person now.
view controls(t: Table, raise_to: State<Float>) {
    row(gap: .sibling, align: .center, wrap: true) {
        if not t.players.any(p -> p.person == invoker) {
            if let seat = free_seat(t) {
                ui.Button("Take a seat", role: .primary) {
                    on press: sit(t, seat + 1)
                }
            } else {
                ui.Text("Every seat is taken")
            }
        } else if in_play(t.hand.street) and t.hand.to_act == position(t, invoker) {
            ui.Button("Fold", icon: .x) {
                on press: fold(t)
            }
            ui.Button(
                if to_call(t, invoker) > 0 { "Call {to_call(t, invoker)}" } else { "Check" },
                icon: .check,
            ) {
                on press: call(t)
            }
            if can_raise(t, invoker) {
                ui.Slider(
                    bind: raise_to,
                    label: "Bet amount",
                    min: raise_least(t, invoker).to_float(),
                    max: raise_most(t, invoker).to_float(),
                    step: big_blind().to_float(),
                    shows_value: true,
                )
                ui.Button(
                    if t.hand.current_bet > 0 { "Raise" } else { "Bet" },
                    icon: .trending_up,
                ) {
                    on press: raise(t, raise_to)
                }
            }
            ui.Button("All in", icon: .flame) {
                on press: all_in(t)
            }
        } else {
            if not in_play(t.hand.street) {
                if ready(t).len() >= 2 {
                    ui.Button(
                        "Deal next hand",
                        icon: .play,
                        role: if dealer(t) == .some(invoker) { .primary } else { .secondary },
                        opens: if dealer(t) == .some(invoker) {
                            .none
                        } else if let since = t.hand.since {
                            .some(since + deal_grace())
                        } else { .none },
                    ) {
                        on press: deal(t, at: now)
                    }
                }
                if (t.players.find(p -> p.person == invoker)?.chips ?? 1) == 0 {
                    ui.Button("Buy back in", icon: .refresh) {
                        on press: rebuy(t)
                    }
                }
                if let at = position(t, invoker) {
                    if let p = t.hand.players.at(at) {
                        if not p.shown {
                            ui.Button("Show my cards", icon: .eye) {
                                on press: show(t)
                            }
                        }
                    }
                }
            } else if let a = actor(t) {
                if a.player != invoker {
                    ui.Button(
                        "Fold for {a.player.name()}",
                        icon: .timer,
                        opens: if let since = t.hand.since {
                            .some(since + turn_time())
                        } else { .none },
                    ) {
                        on press: skip(t, at: now)
                    }
                }
            }
            ui.Button("Leave the table", icon: .arrow_left) {
                on press: leave(t)
            }
        }
    }
}

/// A table with everyone waiting for a second player.
scenario "waiting for players" -> Table {
    let t = root Table
    let grace = person("Grace")
    sit(t)
    invoke(sit(t, 3), by: grace)
    t
}

/// A hand under way, with the person looking to act.
scenario "a hand under way" -> Table {
    let t = root Table
    let grace = person("Grace")
    sit(t, 1)
    invoke(sit(t, 3), by: grace)
    deal(t)
    t
}

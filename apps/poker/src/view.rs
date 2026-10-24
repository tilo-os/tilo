//! The table as each participant sees it: the seats around the board, their
//! own cards face up and everyone else's face down until the showdown, and
//! the moves open to them now. The hand is the window bar's location; the
//! table draws no title row of its own.

use ui::framework::People;
use ui::i18n::I18nManager;
use ui::prelude::*;
use ui::tokens::Rhythm;
use ui::widget::{ButtonRole, Container, Surface, SurfaceLevel, TextStack};

use crate::card::{CardSize, CardView, Showing};
use crate::game::{self, BIG_BLIND, InHand, Street, Table};

/// The seats along the top of the table, then along the bottom, so the
/// order runs clockwise.
const TOP: [usize; 3] = [0, 1, 2];
const BOTTOM: [usize; 3] = [5, 4, 3];
/// A seat is as wide as this many of the cards it holds, so every seat is as
/// wide as the others and the table keeps its shape.
const SEAT_CARDS: u8 = 4;

fn fmt(key: &str, args: &[(&str, ui::i18n::ArgValue)]) -> String {
    I18nManager::format(key, args)
}

fn plain(key: &str) -> String {
    I18nManager::lookup_plain(key)
}

/// The whole table, built again whenever the record changes.
pub fn table(cx: &mut ViewCx<Table>, table: &Table) -> Element {
    let me = cx.me().map(str::to_owned);
    let people = cx.people();
    let me = me.as_deref();
    let seating = table.seating();
    let top = seat_row(cx, table, &seating, &people, me, &TOP);
    let bottom = seat_row(cx, table, &seating, &people, me, &BOTTOM);
    let mut column = Column::new()
        .with_rhythm(Rhythm::Sibling)
        .with_intrinsic_height();
    for part in [
        top,
        board(table),
        bottom,
        status(table, &people, me),
        controls(cx, table, me),
    ] {
        column = column.push(part);
    }
    let centred = Container::new()
        .readable()
        .centered()
        .with_child(Box::new(column));
    Box::new(TableWindow {
        content: Box::new(Scrollable::new_vertical().with_child(Box::new(centred))),
        location: location(table),
    })
}

/// Where the window is: the table, or the hand being played at it.
pub fn location(table: &Table) -> String {
    if table.hand.number == 0 {
        plain("poker-title")
    } else {
        fmt("poker-hand-number", &[("number", table.hand.number.into())])
    }
}

/// The table, with the hand as the window bar's location.
struct TableWindow {
    content: Element,
    location: String,
}

impl TableWindow {
    fn declare(&self) {
        ui::window_toolbar::declare(Some(ui::window_toolbar::WindowToolbar {
            location: Some(ui::window_toolbar::ToolbarLocation {
                label: self.location.clone(),
                ..ui::window_toolbar::ToolbarLocation::default()
            }),
            ..ui::window_toolbar::WindowToolbar::default()
        }));
    }
}

impl ui::widget::Decorator for TableWindow {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.declare();
        self.content.sync(state, path);
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        self.declare();
        self.content.update(state, path)
    }

    fn name(&self) -> &'static str {
        "TableWindow"
    }
}

fn seat_row(
    cx: &ViewCx<Table>,
    table: &Table,
    seating: &[Option<String>; game::SEATS],
    people: &People,
    me: Option<&str>,
    seats: &[usize],
) -> Element {
    let mut row = Row::new()
        .with_rhythm(Rhythm::Sibling)
        .with_justify(ui::taffy::JustifyContent::Center)
        .with_align_items(ui::taffy::AlignItems::Start);
    for seat in seats {
        let occupant = seating.get(*seat).cloned().flatten();
        row = row.push(seat_view(cx, table, *seat, occupant.as_deref(), people, me));
    }
    Box::new(row)
}

fn seat_view(
    cx: &ViewCx<Table>,
    table: &Table,
    seat: usize,
    occupant: Option<&str>,
    people: &People,
    me: Option<&str>,
) -> Element {
    let number = seat.saturating_add(1);
    let Some(player) = occupant else {
        let label = fmt("poker-seat-open", &[("seat", number.into())]);
        let mut body =
            ui::column![TextStack::new(label).secondary(plain("poker-seat-open-detail"))]
                .with_rhythm(Rhythm::Sibling)
                .with_no_padding()
                .with_intrinsic_height();
        let seated = me.is_some_and(|me| table.players.contains_key(me));
        if !seated && !cx.read_only() {
            body = body.push(Box::new(
                cx.button(
                    "poker-sit-here",
                    Table::SIT.with(u32::try_from(number).unwrap_or(1)),
                )
                .enabled_from(&cx.arrived_state()),
            ));
        }
        return seat_box(Box::new(body));
    };
    let name = people.name(player);
    let you = me == Some(player);
    let title = if you {
        fmt("poker-seat-you", &[("name", name.as_str().into())])
    } else {
        name.clone()
    };
    let hand = &table.hand;
    let in_hand = hand.players.iter().position(|p| p.player == player);
    let to_act = hand.street.in_play() && hand.actor().is_some_and(|p| p.player == player);
    let chips = fmt("poker-chips", &[("chips", table.chips(player).into())]);
    let mut stack = TextStack::new(title).secondary(chips);
    if let Some(state) = in_hand
        .and_then(|at| hand.players.get(at))
        .and_then(|p| seat_state(table, p, to_act, you))
    {
        stack = stack.tertiary(state);
    }
    let avatar = ui::widget::Avatar::person(name, player.to_owned());
    let mut body = ui::column![
        ui::row![avatar, stack]
            .with_rhythm(Rhythm::Sibling)
            .no_wrap()
    ]
    .with_rhythm(Rhythm::Sibling)
    .with_no_padding()
    .with_intrinsic_height();
    if let Some(p) = in_hand.and_then(|at| hand.players.get(at)) {
        body = body.push(hole_cards(table, p, you, number));
    }
    seat_box(Box::new(body))
}

fn seat_box(body: Element) -> Element {
    // The surface sizes to what it holds: the content's own width is fixed,
    // so every seat is as wide as the others.
    let content = Container::new()
        .with_no_padding()
        .with_width(f32::from(SEAT_CARDS) * CardSize::Small.width())
        .with_child(Box::new(
            Column::new()
                .push(body)
                .with_no_padding()
                .with_intrinsic_height(),
        ));
    Box::new(Surface::new(SurfaceLevel::Raised, Box::new(content)))
}

/// What the seat says about the player's part in the hand, if anything.
fn seat_state(table: &Table, p: &InHand, to_act: bool, you: bool) -> Option<String> {
    let hand = &table.hand;
    if hand.street == Street::Done {
        if let Some(award) = hand.awards.iter().find(|a| a.player == p.player) {
            return Some(fmt("poker-seat-won", &[("amount", award.amount.into())]));
        }
        return p.folded.then(|| plain("poker-move-fold"));
    }
    if to_act {
        return Some(plain(if you {
            "poker-seat-your-turn"
        } else {
            "poker-seat-their-turn"
        }));
    }
    let button = usize::try_from(hand.button).ok();
    let moved = p.last.message().map(|key| {
        if p.bet > 0 && !p.folded {
            fmt(
                "poker-seat-bet",
                &[("move", plain(key).into()), ("amount", p.bet.into())],
            )
        } else {
            plain(key)
        }
    });
    moved.or_else(|| (hand.position(&p.player) == button).then(|| plain("poker-seat-dealer")))
}

fn hole_cards(table: &Table, p: &InHand, you: bool, seat: usize) -> Element {
    let mut row = Row::new().with_rhythm(Rhythm::Related).no_wrap();
    let open = you || p.shown;
    for (index, card) in p.cards.iter().enumerate() {
        let id = format!("seat-{seat}-card-{}", index.saturating_add(1));
        let showing = if open {
            Showing::Face(*card)
        } else if p.folded && table.hand.street.in_play() {
            continue;
        } else {
            Showing::Back(ui::presence::user_color(&p.player))
        };
        row = row.push(Box::new(CardView::new(id, showing, CardSize::Small)));
    }
    Box::new(row)
}

fn board(table: &Table) -> Element {
    let hand = &table.hand;
    let mut cards = Row::new()
        .with_rhythm(Rhythm::Related)
        .with_justify(ui::taffy::JustifyContent::Center);
    for at in 0..5 {
        let showing = hand
            .board
            .get(at)
            .map_or(Showing::Slot, |card| Showing::Face(*card));
        cards = cards.push(Box::new(CardView::new(
            format!("board-{}", at.saturating_add(1)),
            showing,
            CardSize::Large,
        )));
    }
    let pot = fmt("poker-pot", &[("pot", hand.pot().into())]);
    let street = plain(hand.street.message());
    let blinds = fmt(
        "poker-blinds",
        &[
            ("small", game::SMALL_BLIND.into()),
            ("big", BIG_BLIND.into()),
        ],
    );
    let facts = ui::row![
        Text::new(&street)
            .with_id("street")
            .with_role(TextRole::Label),
        Text::new(&pot).with_id("pot").with_role(TextRole::Heading),
        Text::new(&blinds)
            .with_id("blinds")
            .with_role(TextRole::Caption),
    ]
    .with_rhythm(Rhythm::Sibling)
    .with_justify(ui::taffy::JustifyContent::Center)
    .with_align_items(ui::taffy::AlignItems::Baseline);
    Box::new(Surface::new(
        SurfaceLevel::Region,
        Box::new(
            ui::column![cards, facts]
                .with_rhythm(Rhythm::Sibling)
                .with_intrinsic_height(),
        ),
    ))
}

/// One line on where the hand stands, read out as it changes.
fn status(table: &Table, people: &People, me: Option<&str>) -> Element {
    let hand = &table.hand;
    let mut lines: Vec<String> = Vec::new();
    match hand.street {
        Street::Idle => lines.push(waiting(table, people, me)),
        Street::Done => {
            for award in &hand.awards {
                let name = people.name(&award.player);
                lines.push(if award.category.is_empty() {
                    fmt(
                        "poker-won-uncontested",
                        &[("name", name.into()), ("amount", award.amount.into())],
                    )
                } else {
                    fmt(
                        "poker-won-with",
                        &[
                            ("name", name.into()),
                            ("amount", award.amount.into()),
                            ("hand", plain(&award.category).into()),
                        ],
                    )
                });
            }
            lines.push(waiting(table, people, me));
        }
        _ => {
            if let Some(actor) = hand.actor() {
                lines.push(if Some(actor.player.as_str()) == me {
                    let call = hand.to_call(&actor.player);
                    if call > 0 {
                        fmt("poker-status-you-face", &[("amount", call.into())])
                    } else {
                        plain("poker-status-your-turn")
                    }
                } else {
                    fmt(
                        "poker-status-their-turn",
                        &[("name", people.name(&actor.player).into())],
                    )
                });
            }
        }
    }
    let mut column = Column::new()
        .with_rhythm(Rhythm::Related)
        .with_intrinsic_height();
    for (at, line) in lines.iter().enumerate() {
        column = column.push(Box::new(
            Text::new(line)
                .with_id(format!("status-{}", at.saturating_add(1)))
                .with_live(ui::a11y::Politeness::Polite),
        ));
    }
    Box::new(column)
}

/// What the table waits for between hands.
fn waiting(table: &Table, people: &People, me: Option<&str>) -> String {
    if table.ready().len() < 2 {
        return plain("poker-status-need-players");
    }
    match table.dealer() {
        Some(dealer) if Some(dealer) == me => plain("poker-status-you-deal"),
        Some(dealer) => fmt(
            "poker-status-waiting-dealer",
            &[("name", people.name(dealer).into())],
        ),
        None => plain("poker-status-need-players"),
    }
}

/// The moves open to this participant now.
fn controls(cx: &ViewCx<Table>, table: &Table, me: Option<&str>) -> Element {
    let mut row = Row::new()
        .with_rhythm(Rhythm::Sibling)
        .with_align_items(ui::taffy::AlignItems::Center);
    let Some(me) = me else {
        return Box::new(row);
    };
    let hand = &table.hand;
    if !table.players.contains_key(me) {
        if let Some(seat) = table.free_seat() {
            let seat = u32::try_from(seat.saturating_add(1)).unwrap_or(1);
            // Watching is where everyone starts; sitting down is theirs to
            // choose, once the table they choose a seat at has arrived.
            row = row.push(Box::new(
                cx.button("poker-take-seat", Table::SIT.with(seat))
                    .role(ButtonRole::Primary)
                    .enabled_from(&cx.arrived_state()),
            ));
        } else {
            row = row.push(Box::new(Text::new(&plain("poker-status-table-full"))));
        }
        return Box::new(row);
    }
    let my_turn = hand.street.in_play() && hand.actor().is_some_and(|p| p.player == me);
    if my_turn {
        let call = hand.to_call(me);
        row = row.push(Box::new(
            cx.button("poker-action-fold", Table::FOLD.invoke()),
        ));
        let call_label = if call > 0 {
            fmt("poker-action-call", &[("amount", call.into())])
        } else {
            plain("poker-action-check")
        };
        row = row.push(Box::new(cx.button(call_label, Table::CALL.invoke())));
        if let Some((least, most)) = hand.raise_range(me) {
            #[allow(clippy::cast_precision_loss, reason = "chip counts are small")]
            let (least, most) = (least as f64, most as f64);
            let form = cx.form::<game::table::Raise>();
            let slider = cx.control(
                |amount: &LocalState<f64>| {
                    if amount.get() < least {
                        amount.set(least);
                    }
                    Slider::from_state(amount)
                        .range(least, most)
                        .step(f64::from(u32::try_from(BIG_BLIND).unwrap_or(10)))
                        .shows_value()
                        .with_id("raise-amount")
                        .with_accessible_name(&plain("poker-action-raise-amount"))
                },
                form.paths().to,
            );
            let label = if hand.current_bet > 0 {
                "poker-action-raise"
            } else {
                "poker-action-bet"
            };
            row = row.push(Box::new(slider));
            row = row.push(Box::new(cx.button(label, form.submit())));
        }
        row = row.push(Box::new(
            cx.button("poker-action-all-in", Table::ALL_IN.invoke()),
        ));
        return Box::new(row);
    }
    if !hand.street.in_play() {
        if table.ready().len() >= 2 {
            // The dealer deals; anyone may once the table has waited a while.
            let deal = cx.button("poker-action-deal", Table::DEAL.invoke());
            let deal = if table.dealer() == Some(me) {
                deal.role(ButtonRole::Primary)
            } else {
                deal
            };
            row = row.push(Box::new(deal));
        }
        if table.players.get(me).is_some_and(|p| p.chips == 0) {
            row = row.push(Box::new(
                cx.button("poker-action-rebuy", Table::REBUY.invoke()),
            ));
        }
        if hand.street == Street::Done && hand.players.iter().any(|p| p.player == me && !p.shown) {
            row = row.push(Box::new(
                cx.button("poker-action-show", Table::SHOW.invoke()),
            ));
        }
    } else if hand.actor().is_some() {
        row = row.push(Box::new(
            cx.button("poker-action-skip", Table::SKIP.invoke()),
        ));
    }
    row = row.push(Box::new(
        cx.button("poker-action-leave", Table::LEAVE.invoke()),
    ));
    Box::new(row)
}

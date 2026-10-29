//! Lanes: the items of a collection grouped by a field, each group a filter that compares the
//! field with a value the view holds (`b.cards.filter(c -> c.column == col)`). The index
//! answers each from the field's column, where the items holding one value are one run: a view
//! showing every lane with its count and first card reads per lane the run's bounds and the
//! page it starts on, however many cards the board holds, and checks no card one by one.

use automerge::AutoCommit;
use tilolang_runtime::{InvokeOutcome, Principal, Value};

use crate::fixtures::Replica;

const BOARD: &str = r#"app so.tilo.lang.lanes "Lanes"

type Column {
    title: Text
}

type Card {
    title: Text
    column: Column
}

type Board {
    columns: Collection<Column>
    cards: Collection<Card>
}

root Board

default view board for Board

action add_column(b: Board, title: Text) -> Column {
    b.columns.insert(Column(title: title))
}

action add_card(b: Board, title: Text, col: Column) -> Card {
    b.cards.insert(Card(title: title, column: col))
}

action move_to(card: Card, col: Column) {
    card.column = col
}

view board(b: Board) {
    stack {
        for col in b.columns {
            let cards = b.cards.filter(c -> c.column == col)
            text("{col.title}: {cards.len()}")
            match cards.first() {
                .some(c) => text("first {c.title}"),
                .none => text("none"),
            }
        }
    }
}
"#;

const APP: &str = "so.tilo.lang.lanes";

fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    let meta = r.host.package(APP).unwrap().meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap()
        .id
        .clone();
    match r
        .host
        .runtime()
        .invoke_action(&Principal::new(&r.name), (id.as_str(), args))
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("{action}: {other:?}"),
    }
}

fn texts(node: &tilolang_runtime::UiNode, out: &mut Vec<String>) {
    out.extend(node.props.values().filter_map(|value| match value {
        Value::Text(text) => Some(text.clone()),
        _ => None,
    }));
    for child in &node.children {
        texts(child, out);
    }
}

fn shown(view: &tilolang_runtime::ViewInstance) -> Vec<String> {
    let mut out = Vec::new();
    texts(view.tree(), &mut out);
    out
}

/// A board of three columns with `per` cards in each, added round the columns, and its
/// columns.
fn board(per: usize) -> (Replica, Vec<Value>, Vec<Value>) {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[(BOARD, "")]);
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    let columns: Vec<Value> = ["To do", "Doing", "Done"]
        .iter()
        .map(|title| {
            let col = call(
                &ada,
                "add_column",
                vec![Value::resource(&root), Value::text(*title)],
            );
            ada.commit();
            col
        })
        .collect();
    let mut cards = Vec::new();
    for i in 0..per.saturating_mul(3) {
        let col = columns[i % 3].clone();
        cards.push(call(
            &ada,
            "add_card",
            vec![
                Value::resource(&root),
                Value::text(format!("card {i}")),
                col,
            ],
        ));
        ada.commit();
    }
    (ada, columns, cards)
}

#[test]
fn each_lane_lists_the_cards_of_its_column_and_follows_a_move() {
    let (mut ada, columns, cards) = board(2);
    let mut view = ada.view(APP);
    assert_eq!(
        shown(&view),
        [
            "To do: 2",
            "first card 0",
            "Doing: 2",
            "first card 1",
            "Done: 2",
            "first card 2"
        ]
    );
    call(&ada, "move_to", vec![cards[0].clone(), columns[2].clone()]);
    ada.commit();
    view.poll_patches().unwrap();
    assert_eq!(
        shown(&view),
        [
            "To do: 1",
            "first card 3",
            "Doing: 2",
            "first card 1",
            "Done: 3",
            "first card 0"
        ]
    );
}

#[test]
fn lanes_read_their_runs_not_every_card() {
    let work = |per: usize| {
        let (ada, _, _) = board(per);
        let before = ada.host.store().read_stats();
        let view = ada.view(APP);
        let after = ada.host.store().read_stats();
        (
            shown(&view),
            after.doc.entries - before.doc.entries,
            after.evaluated - before.evaluated,
            after.index.pages - before.index.pages,
        )
    };
    let (small, small_entries, small_evaluated, small_pages) = work(20);
    let (large, large_entries, large_evaluated, large_pages) = work(400);
    assert_eq!(small.first().map(String::as_str), Some("To do: 20"));
    assert_eq!(large.first().map(String::as_str), Some("To do: 400"));
    assert_eq!((small_entries, large_entries), (0, 0));
    assert_eq!(
        (small_evaluated, large_evaluated),
        (0, 0),
        "no card checked"
    );
    assert!(
        large_pages <= small_pages.saturating_add(3),
        "{large_pages} pages at 1200 cards, {small_pages} at 60"
    );
}

app so.tilo.lang.layoutboard "Board layouts" (icon: .layout_grid, category: .utility)

/// A card on the board.
type Card {
    title: Text (title)
}

/// One lane of the board: its heading and its cards, in order.
type Column {
    title: Text (title)
    cards: Collection<Card>
}

/// A board whose lanes hold cards, some as many as a pane shows and some more.
type Board {
    columns: Collection<Column>
}

root Board

default view board for Board

/// The board as an app shows one: lanes in a page that is the window's, so the lanes are as
/// tall as the pane and their cards scroll under the heading that offers to add another.
view board(b: Board) {
    std.page(b, canvas: true, fill: true) {
        ui.Scroll(across: true) {
            ui.Board {
                for col in b.columns {
                    ui.BoardLane(
                        col,
                        title: col.title,
                        count: col.cards.len(),
                        action: "New card",
                    ) {
                        for card in col.cards {
                            ui.BoardCard(card) {
                                row(card.title)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A board of three lanes: six cards in the first, which a window shows at once; one in the
/// second; and thirty in the third, more than a lane has room for.
scenario "a board" {
    let b = root Board
    let todo = b.columns.insert(Column(title: "To do"))
    let doing = b.columns.insert(Column(title: "Doing"))
    let done = b.columns.insert(Column(title: "Done"))
    todo.cards.insert(Card(title: "Card 1 of six"))
    todo.cards.insert(Card(title: "Card 2 of six"))
    todo.cards.insert(Card(title: "Card 3 of six"))
    todo.cards.insert(Card(title: "Card 4 of six"))
    todo.cards.insert(Card(title: "Card 5 of six"))
    todo.cards.insert(Card(title: "Card 6 of six"))
    doing.cards.insert(Card(title: "The card being worked on"))
    done.cards.insert(Card(title: "Card 1 of thirty"))
    done.cards.insert(Card(title: "Card 2 of thirty"))
    done.cards.insert(Card(title: "Card 3 of thirty"))
    done.cards.insert(Card(title: "Card 4 of thirty"))
    done.cards.insert(Card(title: "Card 5 of thirty"))
    done.cards.insert(Card(title: "Card 6 of thirty"))
    done.cards.insert(Card(title: "Card 7 of thirty"))
    done.cards.insert(Card(title: "Card 8 of thirty"))
    done.cards.insert(Card(title: "Card 9 of thirty"))
    done.cards.insert(Card(title: "Card 10 of thirty"))
    done.cards.insert(Card(title: "Card 11 of thirty"))
    done.cards.insert(Card(title: "Card 12 of thirty"))
    done.cards.insert(Card(title: "Card 13 of thirty"))
    done.cards.insert(Card(title: "Card 14 of thirty"))
    done.cards.insert(Card(title: "Card 15 of thirty"))
    done.cards.insert(Card(title: "Card 16 of thirty"))
    done.cards.insert(Card(title: "Card 17 of thirty"))
    done.cards.insert(Card(title: "Card 18 of thirty"))
    done.cards.insert(Card(title: "Card 19 of thirty"))
    done.cards.insert(Card(title: "Card 20 of thirty"))
    done.cards.insert(Card(title: "Card 21 of thirty"))
    done.cards.insert(Card(title: "Card 22 of thirty"))
    done.cards.insert(Card(title: "Card 23 of thirty"))
    done.cards.insert(Card(title: "Card 24 of thirty"))
    done.cards.insert(Card(title: "Card 25 of thirty"))
    done.cards.insert(Card(title: "Card 26 of thirty"))
    done.cards.insert(Card(title: "Card 27 of thirty"))
    done.cards.insert(Card(title: "Card 28 of thirty"))
    done.cards.insert(Card(title: "Card 29 of thirty"))
    done.cards.insert(Card(title: "Card 30 of thirty"))
}

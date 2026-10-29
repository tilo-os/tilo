app so.tilo.lang.board "Board" (icon: .layout_grid, category: .office)

/// A card: one thing to get done, with notes, in one column of the board.
type Card {
    title: Text (title)
    notes: Text (body)
    /// The column it is in.
    column: Column
}

/// A column of the board: a stage the cards go through, such as "Doing".
type Column {
    title: Text (title)
}

/// The workspace's board: its columns, left to right, and its cards, each in one of them.
type Board {
    columns: Collection<Column>
    /// Every card, in the order the columns show them.
    cards: Collection<Card>
}

root Board

default view board for Board

default view card_page for Card

/// Adds a column at the right of the board.
primary action new_column(b: Board) -> Column (icon: .plus, label: "New column") {
    b.columns.insert(Column(title: "Untitled"))
}

/// Adds a card at the foot of a column.
action new_card(b: Board, col: Column) -> Card (icon: .plus, label: "New card") {
    b.cards.insert(Card(title: "Untitled", column: col))
}

/// Moves a card to the foot of a column.
action move_to(b: Board, card: Card, col: Column) (icon: .arrow_right, label: "Move to {col.title}") {
    card.column = col
    if let last = b.cards.last() { b.cards.move(card, after: last) }
}

/// Moves a card to just above another, into that card's column.
action move_before(b: Board, card: Card, next: Card) {
    card.column = next.column
    b.cards.move(card, before: next)
}

/// Deletes a card.
destructive action delete_card(b: Board, card: Card) (icon: .trash, label: "Delete") {
    b.cards.remove(card)
}

/// Renames a column.
action rename_column(col: Column, title: Text) {
    col.title = title
}

/// The cards of a column, in order.
fn cards_of(b: Board, col: Column) -> List<Card> {
    b.cards.filter(c -> c.column == col)
}

/// The board's window: a lane per column with its cards, each card its title and the start of
/// its notes. A card is dragged to another place or column, or moved from its menu, which also
/// holds its actions, and opens in a page of its own.
view board(b: Board) {
    std.page(b, canvas: true, empty: b.columns.is_empty()) {
        ui.Scroll(across: true) {
            ui.Board {
                for col in b.columns {
                    let cards = cards_of(b, col)
                    ui.BoardLane(col, title: col.title, count: cards.len(), action: "New card") {
                        on act: new_card(b, col)
                        on drop(moved: Card): move_to(b, moved, col)
                        for card in cards {
                            card_tile(b, card)
                        }
                    }
                }
            }
        }
    }
}

/// A card on the board: its title and the start of its notes. It opens in a page, takes a card
/// dropped on it to just above it, and its menu moves it to another column or runs its actions.
view card_tile(b: Board, card: Card) {
    ui.BoardCard(card, title: card.title) {
        on activate: open(card)
        on drop(moved: Card): move_before(b, moved, card)
        menu: {
            for other in b.columns {
                if other != card.column {
                    ui.MenuItem("Move to {other.title}", icon: .arrow_right) {
                        on choose: move_to(b, card, other)
                    }
                }
            }
            for a in actions(card) {
                ui.MenuItem(a.label, icon: a.icon, destructive: a.destructive) {
                    on choose: a.run
                }
            }
        }
        if card.notes != "" {
            text(card.notes, role: .caption)
        }
    }
}

/// A card's page: its title and its notes.
view card_page(card: Card) {
    std.document(card) {
        ui.TextInput(value: card.title, placeholder: "Title")
        ui.TextInput(value: card.notes, placeholder: "Notes", lines: 8)
    }
}

/// A team's board a week into a release: three columns with cards in each.
scenario "a release board" {
    let b = root Board
    let todo = b.columns.insert(Column(title: "To do"))
    let doing = b.columns.insert(Column(title: "Doing"))
    let done = b.columns.insert(Column(title: "Done"))
    b.cards.insert(Card(title: "Write the release notes", notes: "Lead with the sync fix, then the new board.", column: todo))
    b.cards.insert(Card(title: "Screenshots for the docs", column: todo))
    b.cards.insert(Card(title: "Translate the onboarding", notes: "German and French first.", column: todo))
    b.cards.insert(Card(title: "Check the importer on large tables", column: todo))
    b.cards.insert(Card(title: "Fix the sidebar at large text", notes: "It collapses too early above 150%.", column: doing))
    b.cards.insert(Card(title: "Review the sharing dialog", column: doing))
    b.cards.insert(Card(title: "Bump the version", column: done))
    b.cards.insert(Card(title: "Merge the offline queue", notes: "Landed on Tuesday.", column: done))
    b.cards.insert(Card(title: "Plan the next cycle", column: done))
}

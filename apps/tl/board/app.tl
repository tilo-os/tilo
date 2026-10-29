app so.tilo.lang.board "Board" (icon: .layout_grid, category: .office)

/// A card: one thing to get done, with notes, in one column of the board.
type Card {
    title: Text (title)
    notes: Text (body)
    /// The column it was in, before cards were kept in their columns.
    column: Column (previously)
}

/// A column of the board: a stage the cards go through, such as "Doing", with its cards in
/// order.
type Column {
    title: Text (title)
    cards: Collection<Card>
}

/// The workspace's board: its columns, left to right, each with its cards.
type Board {
    columns: Collection<Column>
    /// Every card, before cards were kept in their columns.
    cards: Collection<Card> (previously)
}

root Board

default view board for Board

default view card_page for Card

/// Puts each card of a board from before cards were kept in their columns into its column, in
/// the order the board showed them.
migration action place_cards(b: Board) {
    for card in b.cards {
        let col = card.column
        col.cards.move(card, to: col.cards.len())
    }
}

/// Adds a column at the right of the board.
primary action new_column(b: Board) -> Column (icon: .plus, label: "New column") {
    b.columns.insert(Column(title: "Untitled"))
}

/// Adds a card at the foot of a column.
action new_card(col: Column) -> Card (icon: .plus, label: "New card") {
    col.cards.insert(Card(title: "Untitled"))
}

/// Moves a card to the foot of a column.
action move_to(card: Card, col: Column) (icon: .arrow_right, label: "Move to {col.title}") {
    col.cards.move(card, to: col.cards.len())
}

/// Moves a card to just above another, in that card's column.
action move_before(col: Column, card: Card, next: Card) {
    col.cards.move(card, before: next)
}

/// Deletes a card.
destructive action delete_card(col: Column, card: Card) (icon: .trash, label: "Delete") {
    col.cards.remove(card)
}

/// Renames a column.
action rename_column(col: Column, title: Text) {
    col.title = title
}

/// The board's window: a lane per column with its cards, each card its title and the start of
/// its notes. A card is dragged to another place or column, or moved from its menu, which also
/// holds its actions, and opens in a page of its own.
view board(b: Board) {
    std.page(b, canvas: true, empty: b.columns.is_empty()) {
        ui.Scroll(across: true) {
            ui.Board {
                for col in b.columns {
                    ui.BoardLane(col, title: col.title, count: col.cards.len(), action: "New card") {
                        on act: new_card(col)
                        on drop(moved: Card): move_to(moved, col)
                        for card in col.cards {
                            card_tile(b, col, card)
                        }
                    }
                }
            }
        }
    }
}

/// A card on the board: its title and the start of its notes. It opens in a page, takes a card
/// dropped on it to just above it, and its menu moves it to another column or runs its actions.
view card_tile(b: Board, col: Column, card: Card) {
    ui.BoardCard(card, title: card.title) {
        on activate: open(card)
        on drop(moved: Card): move_before(col, moved, card)
        menu: {
            for other in b.columns {
                if other != col {
                    ui.MenuItem("Move to {other.title}", icon: .arrow_right) {
                        on choose: move_to(card, other)
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
    todo.cards.insert(Card(title: "Write the release notes", notes: "Lead with the sync fix, then the new board."))
    todo.cards.insert(Card(title: "Screenshots for the docs"))
    todo.cards.insert(Card(title: "Translate the onboarding", notes: "German and French first."))
    todo.cards.insert(Card(title: "Check the importer on large tables"))
    doing.cards.insert(Card(title: "Fix the sidebar at large text", notes: "It collapses too early above 150%."))
    doing.cards.insert(Card(title: "Review the sharing dialog"))
    done.cards.insert(Card(title: "Bump the version"))
    done.cards.insert(Card(title: "Merge the offline queue", notes: "Landed on Tuesday."))
    done.cards.insert(Card(title: "Plan the next cycle"))
}

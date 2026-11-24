app so.tilo.lang.layoutdetail "Detail" (icon: .layout_grid, category: .utility)

/// One item of the list, with the box its row checks it off by.
type Item {
    title: Text
    done: Bool = false

    search {
        filter "All" = true
        filter "Done" = done
    }
}

/// The list a window shows: its items, one a person opens and checks off.
type Notebook {
    items: Collection<Item>
}

root Notebook

default view notebook for Notebook

default view item_page for Item

/// Checks an item off, as the box of its row does.
action check(item: Item, done: Bool = not item.done) {
    item.done = done
}

/// The window: the items of the place it shows, each as a row a finger taps.
view notebook(n: Notebook) {
    state place: Filter<Item> = Item.all
    let listed = n.items.matching(place)
    std.list(listed) { (item, _) ->
        ui.ListItem(item: item) {
            item_row(item)
        }
    }
}

/// An item as its row: the title, and the box that checks it off.
view item_row(item: Item) {
    row(item.title, checkbox: item.done, check: check(item))
}

/// An item as the page its row opens.
view item_page(item: Item) {
    std.document(item) {
        ui.Text(item.title)
    }
}

/// A list to tap and check off.
scenario "a list" {
    let n = root Notebook
    n.items.insert(Item(title: "Order boxes"))
    n.items.insert(Item(title: "Book the van"))
}

/// The list with an item opened in front of it, as a window that kept what it showed comes back
/// to it.
scenario "an open item" -> Item {
    let n = root Notebook
    let first = n.items.insert(Item(title: "Order boxes"))
    n.items.insert(Item(title: "Book the van"))
    first
}

app so.tilo.sweep "Sweep"

// A board whose items stand in two collections of the one type, what is on it and what was set
// aside, and items that hold two collections of one type themselves: removing what a list
// operation picked out of one of them.

type Part {
    name: Text
    worn: Bool = false
}

type Item {
    title: Text
    done: Bool = false
    parts: Collection<Part>
    spares: Collection<Part>
}

type Board {
    name: Text
    items: Collection<Item>
    kept: Collection<Item>
}

action add(b: Board, title: Text) -> Item {
    return b.items.insert(Item(title: title))
}

action keep(b: Board, title: Text) -> Item {
    return b.kept.insert(Item(title: title))
}

action finish(i: Item) {
    i.done = true
}

action fit(i: Item, name: Text, worn: Bool) -> Part {
    return i.parts.insert(Part(name: name, worn: worn))
}

action stock(i: Item, name: Text, worn: Bool) -> Part {
    return i.spares.insert(Part(name: name, worn: worn))
}

action clear_filtered(b: Board) {
    for x in b.items.filter(i -> i.done) {
        b.items.remove(x)
    }
}

action clear_where(b: Board) -> Int {
    return b.items.remove_where(i -> i.done)
}

action clear_sorted(b: Board) {
    for x in b.items.sort_by(i -> i.title) {
        if x.done { b.items.remove(x) }
    }
}

action clear_first(b: Board) {
    if let x = b.items.find(i -> i.done) { b.items.remove(x) }
}

action clear_kept(b: Board) {
    for x in b.kept.filter(i -> i.done) {
        b.kept.remove(x)
    }
}

action clear_worn_spares(i: Item) {
    for p in i.spares.filter(p -> p.worn) {
        i.spares.remove(p)
    }
}

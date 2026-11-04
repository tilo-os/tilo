// What an item standing under another can do, in the words the placeholders name it by: the
// item type `%item%`, the type holding its items `%holder%`, the root variable `%root%` (also
// the holder parameter's name), the collection field `%items%`, and the fields the item type's
// markers name: `%parent%`, `%done%`, `%title%`. `%copy%` writes every field of an item as its
// copy's, and `%inherit%` names the fields an item keeps of the one it stands under.
//
// `crate::outline` derives these actions for a type, so no app writes them.
/// Checks an item off with every item under it, or opens it again with the items it is under.
action check(t: %item%, done: Bool = not t.%done%) (
    label: if done { "Complete" } else { "Reopen" },
    icon: .check,
    on: t.%done%,
    key: "Ctrl+Enter",
) {
    t.%done% = done
    if done {
        for x in %root%.%items%.descendants(t, .%parent%) {
            x.%done% = true
        }
    } else {
        // Everything above the one opened is open again.
        for a in %root%.%items%.ancestors(t, .%parent%) {
            a.%done% = false
        }
    }
}

/// Adds a subtask under an item: an item of its own, at the end of the items it holds, under the
/// title given, keeping what it inherits of the one it stands under. An item that was done opens
/// again: there is work under it now.
action add_subtask(%root%: %holder%, t: %item%, title: Text) -> %item% (
    label: "Add subtask",
    icon: .list_tree,
    did: "added a subtask",
    key: "Ctrl+Shift+N",
) {
    let made = %root%.%items%.insert(
        %item%(%title%: title, %parent%: .some(t)%inherit%),
    )
    let last = %root%.%items%.filter(x -> x.%parent% == .some(t) and x != made).last() ?? t
    %root%.%items%.move(made, after: last)
    if t.%done% or %root%.%items%.ancestors(t, .%parent%).any(a -> a.%done%) { check(t, false) }
    made
}

/// Moves an item to a place in the outline: at the end of `parent`'s items (the top level for
/// none), where `before` stands when it is given, or at the end of the top level. An item cannot
/// go inside itself, and open work put under a done item opens it. The outline's drag, `Ctrl+]`
/// and `Ctrl+[` name it; it is not a command of its own.
internal action move_to(%root%: %holder%, moved: %item%, parent: Option<%item%>, before: Option<%item%>) {
    require not within(%root%.%items%, parent, moved)
        and not within(%root%.%items%, before?.%parent%, moved) "An item cannot go inside itself"
    if let b = before {
        moved.%parent% = b.%parent%
        %root%.%items%.move(moved, before: b)
    } else {
        moved.%parent% = parent
        if let last = %root%.%items%.filter(x -> x.%parent% == parent and x != moved).last() {
            %root%.%items%.move(moved, after: last)
        } else if let p = parent { %root%.%items%.move(moved, after: p) }
    }
    if not moved.%done% and %root%.%items%.ancestors(moved, .%parent%).any(a -> a.%done%) {
        check(moved, false)
    }
}

/// Moves an item under the item the view shows just over it in its group, as `Ctrl+]` does: the
/// row over it names the item of its own level it ends up under.
action indent(t: %item%, above: Option<%item%> (above)) (
    label: "Indent",
    icon: .arrow_right,
    key: "Ctrl+]",
) {
    applies above != .none and above != t.%parent% "Nothing over it to move under"
    if let a = above {
        let under = %root%.%items%.ancestors(a, .%parent%).find(x -> x.%parent% == t.%parent%) ?? a
        move_to(%root%, t, .some(under), .none)
    }
}

/// Moves an item out from under its parent, just after it, as `Ctrl+[` does in an outline.
action outdent(t: %item%) (label: "Outdent", icon: .arrow_left, key: "Ctrl+[") {
    applies t.%parent% != .none "It is at the top level already"
    if let p = t.%parent% {
        t.%parent% = p.%parent%
        %root%.%items%.move(t, after: p)
    }
}

/// Copies an item, with every item under it, just after the one it copies.
action duplicate(%root%: %holder%, t: %item%) -> %item% (
    label: "Duplicate",
    icon: .copy,
    key: "Ctrl+Shift+D",
) {
    let copy = %root%.%items%.insert(copied(t, t.%parent%))
    if let last = last_below(%root%.%items%, t) {
        %root%.%items%.move(copy, after: last)
    } else { %root%.%items%.move(copy, after: t) }
    copy_below(%root%, t, copy)
    copy
}

/// Deletes an item and the items under it.
destructive action remove(%root%: %holder%, t: %item%) (
    label: "Delete",
    icon: .trash,
    key: "Delete",
) {
    for x in %root%.%items%.descendants(t, .%parent%) {
        %root%.%items%.remove(x)
    }
    %root%.%items%.remove(t)
}

/// Whether `place` is `top` or one of the items over it.
fn within(items: List<%item%>, place: Option<%item%>, top: %item%) -> Bool {
    if let p = place {
        p == top or items.ancestors(p, .%parent%).any(a -> a == top)
    } else { false }
}

/// `t` as an item of its own under `parent`: what a copy of it says, without the items under it.
fn copied(t: %item%, parent: Option<%item%>) -> %item% {
    %item%(%copy%)
}

/// The item that ends the run `t` begins: the last item of the list that stands under `t`, `t`
/// itself when none does.
fn last_below(items: List<%item%>, t: %item%) -> Option<%item%> {
    items.filter(x -> x == t or items.ancestors(x, .%parent%).any(a -> a == t)).last()
}

/// Copies the items under `t` under `copy`, each in its place, so a copy reads as the item it
/// copies did; answers the last item the copies end with. It is what Duplicate runs, not a
/// command of its own.
internal action copy_below(%root%: %holder%, t: %item%, copy: %item%) -> %item% {
    for k in %root%.%items%.filter(x -> x.%parent% == .some(t)) {
        // Where the copy of this one goes: after the run the copies under `copy` make so far,
        // read before it is inserted, since it would end that run itself.
        let after = last_below(%root%.%items%, copy)
        let made = %root%.%items%.insert(copied(k, .some(copy)))
        if let last = after { %root%.%items%.move(made, after: last) }
        copy_below(%root%, k, made)
    }
    last_below(%root%.%items%, copy) ?? copy
}

app so.tilo.lang.layoutbroken "Broken layouts" (icon: .layout_grid, category: .utility)

/// A sample whose views are each laid out wrong in one way, for `tilo-layout-check`.
type Sample {
    name: Text
}

root Sample

default view samples for Sample

/// Renames the sample.
action rename(s: Sample) {
    s.name = "Renamed"
}

/// Every broken view, one under the other.
view samples(s: Sample) {
    stack {
        overflowing(s)
        clipped(s)
        squeezed(s)
        overlapping(s)
        small_target(s)
        sideways(s)
    }
}

/// A box holding one wider than itself.
view overflowing(s: Sample) {
    stack(max: 6rem) {
        stack(min: 12rem) {
            text("Wider than the box it is in")
        }
    }
}

/// A word wider than the column it is in: it breaks anywhere rather than running past the
/// column, so it is laid out right.
view clipped(s: Sample) {
    stack(max: 6rem) {
        text("Donaudampfschifffahrtsgesellschaft")
    }
}

/// A row that gave each label an equal share of a line too short for them: the row stands wider
/// than any one label's word, so each word had room to stand whole.
view squeezed(s: Sample) {
    row(max: 6rem) {
        text("Created", fill: 1.0)
        text("just now", fill: 1.0)
    }
}

/// Two labels in one spot.
view overlapping(s: Sample) {
    layers {
        text("First label", at: .center)
        text("Second label", at: .center)
    }
}

/// A chip a finger presses: on a touchscreen it takes a finger's room in its line, so it is
/// the one view here laid out right.
view small_target(s: Sample) {
    ui.Chip(s.name) {
        on press: rename(s)
    }
}

/// A row wider than a phone.
view sideways(s: Sample) {
    stack(min: 40rem) {
        text("Forty rem wide")
    }
}

/// A sample with a name.
scenario "a sample" {
    let s = root Sample
    s.name = "Sample"
}

app so.tilo.lang.layoutpixels "Pixel edges" (icon: .layout_grid, category: .utility)

/// A sample whose views are sized in fractions of a rem, so its edges fall between pixels unless
/// layout snaps them, for `tilolang layout-check`.
type Sample {
    name: Text (title)
}

root Sample

default view samples for Sample

/// Boxes a fraction of a rem wide, side by side and in turn, under lines of text whose height
/// is no whole number of pixels at a larger text size.
view samples(s: Sample) {
    stack {
        stack(min: 7.3rem) {
            text("Seven and a bit")
        }
        row {
            stack(min: 3.7rem) {
                text("Three")
            }
            stack(min: 2.9rem) {
                text("Two")
            }
            stack(min: 5.1rem) {
                text("Five")
            }
        }
        text("A paragraph long enough to wrap onto a few lines, so the boxes under it stand at a height in pixels that is not a whole number unless layout rounds it")
        stack(min: 4.45rem) {
            text(s.name)
        }
    }
}

/// A sample with a name.
scenario "a sample" {
    let s = root Sample
    s.name = "Sample"
}

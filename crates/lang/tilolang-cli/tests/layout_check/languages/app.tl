app so.tilo.lang.layoutlanguages "Layout languages" (icon: .layout_grid, category: .utility)

/// A sample whose words ship in two languages, one of them short a message, for the check that
/// a window reads in one language throughout (`tilolang layout-check --locale`).
type Sample {
    name: Text
}

root Sample

default view samples for Sample

/// Every word the sample shows.
view samples(s: Sample) {
    ui.Text("A translated word")
    ui.Text("A word only the source language has")
}

/// A sample with a name.
scenario "a sample" {
    let s = root Sample
    s.name = "Sample"
}

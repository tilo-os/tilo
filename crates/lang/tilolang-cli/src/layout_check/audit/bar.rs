//! The window's bar: what is wrong with how it lays itself out across the window.
//!
//! The bar belongs to the window rather than to the app, so a finding names it as
//! [`BAR`] and reads the bar's own layout, not the widget tree.

use super::{EPSILON, Kind, Problem, SHELL};

/// The window's bar, as a finding names the widget it is about: the bar is the window's, not
/// the app's.
const BAR: &str = "WindowBar";

/// The slot of the strip's own control that opens another application.
const NEW_TAB: &str = "new-tab";

/// What is wrong with one window's bar as it lays itself out across the window: a control it
/// shows twice, and an action that asks for its words standing as its icon alone while the bar
/// has room for those words.
pub fn bar(layout: &ui::window_toolbar::BarLayout) -> Vec<Problem> {
    let mut out = Vec::new();
    for (index, control) in layout.controls.iter().enumerate() {
        // The strip's own door to another application stands before the app's slots and says
        // what it opens: it is no copy of an action of the app's, whatever glyph both wear.
        if control.slot == NEW_TAB {
            continue;
        }
        let Some(later) = layout.controls.get(index.saturating_add(1)..) else {
            continue;
        };
        let Some(second) = later
            .iter()
            .filter(|second| second.slot != NEW_TAB)
            .find(|second| same_control(control, second))
        else {
            continue;
        };
        let what = |control: &ui::window_toolbar::BarControl| {
            if control.name.is_empty() {
                control.id.clone()
            } else {
                format!("\"{}\"", control.name)
            }
        };
        out.push(Problem {
            kind: Kind::DuplicateBarControl,
            keys: Vec::new(),
            widget: BAR.to_owned(),
            id: Some(control.id.clone()),
            detail: format!(
                "the bar shows {} and {} as one control: the {} control at x {:.0} and the {} \
                 control at x {:.0}, where one bar shows each control once and keeps one \"…\" \
                 for what does not fit in it",
                what(control),
                what(second),
                control.slot,
                control.rect[0],
                second.slot,
                second.rect[0],
            ),
        });
    }
    for control in &layout.controls {
        // Drawn as its icon alone, narrower than the words it has, while the room stood empty
        // beside it: a person cannot read what it does. The row's own "…" is a glyph by design.
        if control.slot == "overflow" || control.icon.is_none() || control.words <= 0.0 {
            continue;
        }
        let missing = control.words - control.rect[2];
        if missing <= EPSILON || layout.spare < missing {
            continue;
        }
        out.push(Problem {
            kind: Kind::IconOnlyBarAction,
            keys: Vec::new(),
            widget: BAR.to_owned(),
            id: Some(control.id.clone()),
            detail: format!(
                "the bar draws \"{}\" as its icon alone while {:.0}px of its location stand empty \
                 beside it, and the words need {:.0}px: a bar says what an action does where it \
                 has room for the words",
                control.name, layout.spare, control.words,
            ),
        });
    }
    out
}

/// What is wrong with a window that shows its bar and, under it, a second header row of its
/// own: one bar per window, the app's title, navigation, actions and catch-up standing in it and
/// the window's controls at its end.
pub fn stacked_header_rows(bar_height: f64, header_row: f64) -> Vec<Problem> {
    if bar_height <= EPSILON || header_row <= EPSILON {
        return Vec::new();
    }
    vec![Problem {
        kind: Kind::StackedHeaderRows,
        keys: Vec::new(),
        widget: SHELL.to_owned(),
        id: None,
        detail: format!(
            "the window shows a bar {bar_height:.0}px tall and, under it, a header row of its \
             own {header_row:.0}px tall: one bar per window, with the app's title, navigation \
             and actions standing in it and the window's controls at its trailing end",
        ),
    }]
}

/// Whether one bar shows `a` and `b` as the same control: the same action under two ids, or two
/// controls one glyph or one name apart, which a person cannot tell from each other. One bar
/// shows each control once, and keeps one "…" for what does not fit in it.
fn same_control(a: &ui::window_toolbar::BarControl, b: &ui::window_toolbar::BarControl) -> bool {
    if !a.id.is_empty() && a.id == b.id {
        return true;
    }
    if a.icon.is_some() || b.icon.is_some() {
        return a.icon == b.icon;
    }
    !a.name.is_empty() && a.name == b.name
}

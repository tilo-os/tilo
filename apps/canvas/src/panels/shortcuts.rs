//! The keyboard shortcuts, as `?` shows them: one table the canvas's key
//! handling is tested against, so the overview never lists a key that does
//! nothing.

use ui::prelude::*;

use super::caption;
use crate::tools::Tool;

/// One shortcut: the keys as a person presses them and what they do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    /// The keys, `+`-joined: `Ctrl+Shift+G`, `Shift+1`, `Space+drag`.
    pub keys: &'static str,
    /// The Fluent id of what it does.
    pub does: &'static str,
}

pub struct Group {
    /// The Fluent id of the heading.
    pub heading: &'static str,
    pub shortcuts: &'static [Shortcut],
}

const fn key(keys: &'static str, does: &'static str) -> Shortcut {
    Shortcut { keys, does }
}

/// Every shortcut, grouped. The tools' own keys are [`Tool::shortcut`]'s.
pub const GROUPS: [Group; 4] = [
    Group {
        heading: "canvas-keys-artwork",
        shortcuts: &[
            key("Ctrl+O", "canvas-keys-open"),
            key("Ctrl+S", "canvas-keys-save"),
            key("Ctrl+Shift+S", "canvas-keys-save-as"),
            key("Ctrl+Shift+E", "canvas-keys-export"),
            key("Ctrl+Z", "canvas-keys-undo"),
            key("Ctrl+Shift+Z", "canvas-keys-redo"),
            key("Ctrl+Shift+H", "canvas-keys-history"),
            key("Ctrl+K", "canvas-keys-palette"),
            key("?", "canvas-keys-shortcuts"),
        ],
    },
    Group {
        heading: "canvas-keys-view",
        shortcuts: &[
            key("+", "canvas-keys-zoom-in"),
            key("-", "canvas-keys-zoom-out"),
            key("Shift+1", "canvas-keys-fit"),
            key("Shift+0", "canvas-keys-actual"),
            key("Shift+2", "canvas-keys-zoom-selection"),
            key("Ctrl+scroll", "canvas-keys-zoom-pointer"),
            key("Space+drag", "canvas-keys-pan"),
            key("[", "canvas-keys-brush-smaller"),
            key("]", "canvas-keys-brush-larger"),
        ],
    },
    Group {
        heading: "canvas-keys-marks",
        shortcuts: &[
            key("Ctrl+A", "canvas-keys-select-all"),
            key("Ctrl+C", "canvas-keys-copy"),
            key("Ctrl+X", "canvas-keys-cut"),
            key("Ctrl+V", "canvas-keys-paste"),
            key("Ctrl+D", "canvas-keys-duplicate"),
            key("Ctrl+G", "canvas-keys-group"),
            key("Ctrl+Shift+G", "canvas-keys-ungroup"),
            key("Arrows", "canvas-keys-nudge"),
            key("Shift+Arrows", "canvas-keys-nudge-far"),
            key("Delete", "canvas-keys-delete"),
            key("Enter", "canvas-keys-enter"),
            key("Escape", "canvas-keys-escape"),
            key("Alt+drag", "canvas-keys-alt-drag"),
            key("Shift+drag", "canvas-keys-shift-drag"),
        ],
    },
    Group {
        heading: "canvas-keys-tools",
        shortcuts: &[],
    },
];

/// The keys that do `does` (a Fluent id of [`GROUPS`]), as a menu row
/// shows them beside the same command.
pub fn keys_for(does: &str) -> Option<&'static str> {
    GROUPS
        .iter()
        .flat_map(|group| group.shortcuts.iter())
        .find(|shortcut| shortcut.does == does)
        .map(|shortcut| shortcut.keys)
}

/// The tools and the keys that pick them up.
pub fn tools() -> Vec<(String, &'static str)> {
    Tool::ALL
        .iter()
        .map(|tool| (tool.shortcut().to_owned(), tool.label()))
        .collect()
}

/// The keys as shown: each key in its own cap.
fn keycaps(keys: &str) -> Element {
    let mut row = Row::new()
        .no_wrap()
        .with_rhythm(ui::tokens::Rhythm::Tight)
        .with_align_items(ui::taffy::AlignItems::Center);
    let parts: Vec<&str> = if keys == "+" {
        vec!["+"]
    } else {
        keys.split('+').collect()
    };
    for part in parts {
        let shown = match part {
            "Arrows" => "← ↑ → ↓".to_owned(),
            "drag" | "scroll" => {
                ui::i18n::I18nManager::lookup_plain(&format!("canvas-keys-{part}"))
            }
            other => other.to_owned(),
        };
        row = row.push(Box::new(
            ui::widget::Badge::text(shown).style(ui::widget::ChipStyle::Neutral),
        ));
    }
    super::layout::width(
        Box::new(Container::new().with_no_padding().with_child(Box::new(row))),
        |metrics| metrics.panel_width_list / 3.0,
    )
}

fn line(keys: &str, does: &str) -> Element {
    Box::new(
        Row::new()
            .no_wrap()
            .with_rhythm(ui::tokens::Rhythm::Sibling)
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(keycaps(keys))
            .push(Box::new(Text::new(does).with_role(TextRole::Label))),
    )
}

/// One group: its heading, then a line per shortcut.
fn group(heading: &str, lines: Vec<Element>) -> Element {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Tight)
        .push(super::heading(heading));
    for line in lines {
        column = column.push(line);
    }
    Box::new(column)
}

/// The overview: the artwork and view keys in one column, the marks' in
/// the other, and under them the tools' letters in a row.
pub fn overview() -> Element {
    let column = || {
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Sibling)
    };
    let (mut left, mut right) = (column(), column());
    let mut tools_heading = "";
    for (index, g) in GROUPS.iter().enumerate() {
        if g.shortcuts.is_empty() {
            tools_heading = g.heading;
            continue;
        }
        let lines = g
            .shortcuts
            .iter()
            .map(|shortcut| line(shortcut.keys, shortcut.does))
            .collect();
        if index < 2 {
            left = left.push(group(g.heading, lines));
        } else {
            right = right.push(group(g.heading, lines));
        }
    }
    let mut letters = Row::new().with_rhythm(ui::tokens::Rhythm::Sibling);
    for (keys, does) in tools() {
        letters = letters.push(Box::new(
            Row::new()
                .no_wrap()
                .with_rhythm(ui::tokens::Rhythm::Related)
                .with_align_items(ui::taffy::AlignItems::Center)
                .push(Box::new(
                    ui::widget::Badge::text(keys).style(ui::widget::ChipStyle::Neutral),
                ))
                .push(Box::new(Text::new(does).with_role(TextRole::Label))),
        ));
    }
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Sibling)
            .push(Box::new(
                Row::new()
                    .no_wrap()
                    .with_rhythm(ui::tokens::Rhythm::Section)
                    .with_align_items(ui::taffy::AlignItems::Start)
                    .push(Box::new(left))
                    .push(Box::new(right)),
            ))
            .push(super::heading(tools_heading))
            .push(Box::new(letters))
            .push(caption(&ui::i18n::I18nManager::lookup_plain(
                "canvas-keys-focus",
            ))),
    )
}

//! A shared document's reactions in its window's bar: the pills at the
//! row's end before the window's controls, the person's own one chosen,
//! and the add button opening the emoji picker under it.

use ui::window_toolbar::{ToolbarLocation, ToolbarReaction, ToolbarReactions, WindowToolbar};

use super::{Action, BarTab, Scenario};

/// The Word Processor's bar on a document three people reacted to, the
/// person among them.
fn reacted_document_slots() -> WindowToolbar {
    let pill = |emoji: &str, count: u32, mine: bool, names: &[&str]| ToolbarReaction {
        emoji: emoji.into(),
        count,
        mine,
        names: names.iter().map(|name| (*name).into()).collect(),
    };
    WindowToolbar {
        location: Some(ToolbarLocation {
            label: "Quarterly planning summary".into(),
            ..ToolbarLocation::default()
        }),
        reactions: Some(ToolbarReactions {
            pills: vec![
                pill("🎉", 3, true, &["Ada", "Grace"]),
                pill("👍", 1, false, &["Ada"]),
            ],
            can_react: true,
        }),
        ..WindowToolbar::default()
    }
}

const BAR_DOCUMENT: BarTab = BarTab {
    app_id: "so.tilo.word-processor",
    title: "Quarterly planning summary",
    surface_id: 9105,
    slots: Some(reacted_document_slots),
};

pub(super) fn window_bar_reactions() -> Scenario {
    Scenario {
        name: "window-bar-reactions",
        description: "A shared document's reactions in its window's bar: the \
             pills at the row's end before the window's controls, the person's \
             own one chosen; the add button opens the emoji picker under it.",
        actions: vec![
            Action::SeedWindowBar {
                tabs: &[BAR_DOCUMENT],
                active: 0,
                width: 700,
            },
            Action::Wait(30),
            Action::Relayout,
            Action::Snapshot("01-reactions"),
            Action::ClickNamed("window_reaction_add"),
            Action::Wait(20),
            Action::Snapshot("02-picker"),
        ],
    }
}

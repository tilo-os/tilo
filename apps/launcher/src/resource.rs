//! The launcher as a resource: the door its effects are published on, so
//! the person, the palette, an agent and a script open search, type into
//! it, narrow it and choose what it found through the same actions.
//!
//! The launcher keeps no document. What is being searched (the query, the
//! scope, the kinds chosen) and what was found live in the window for as
//! long as it is open and are published as its live rows; nothing of it is
//! recorded, synced or kept in History.

use ui::framework::{ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{self, Effect, Model, resource};
use ui::widget::Element;

/// The launcher resource: the door its effects and result rows are
/// published on. It holds nothing.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Launcher {}

#[resource(kind = "launcher.search", label = "Search")]
impl Launcher {}

/// What anyone can do with the launcher. Opening, typing, narrowing and
/// moving the selection only change what the launcher shows. Choosing a row,
/// stepping its value or answering the step it asks for runs what the row
/// stands for as the person: whoever acts for them is asked first, as the
/// row may do anything the person can.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.launcher.search")]
pub enum LauncherEffect {
    /// Open search
    #[effect(view, own_device, idempotent, icon = "search")]
    Open {
        #[param("Where to search: app (the app in front), workspace or everywhere")]
        scope: Option<String>,
        #[param("What to search for")]
        query: Option<String>,
    },
    /// Search for
    #[effect(view, own_device, idempotent)]
    Search {
        #[param("What to search for")]
        query: String,
    },
    /// Show only results of kinds
    #[effect(view, own_device, idempotent)]
    Filter {
        #[param(
            "The kinds of result to show (Applications, Files, Actions…); none shows every kind"
        )]
        kinds: Vec<String>,
    },
    /// Search one place wider
    #[effect(view, own_device)]
    Widen,
    /// Move the selection
    #[effect(view, own_device)]
    MoveSelection {
        #[param("Rows to move by; negative moves up")]
        by: i64,
    },
    /// Choose result
    #[effect(own_device, attended)]
    Choose {
        #[param("The result to choose; the selected one when left out")]
        row: Option<String>,
        #[param("How to open it: here (as a tab here), beside, or more (its other actions)")]
        how: Option<String>,
    },
    /// Step the value of a result
    #[effect(own_device, attended)]
    Adjust {
        #[param("The result whose value to step; the selected one when left out")]
        row: Option<String>,
        #[param("1 steps up, -1 down")]
        delta: i64,
    },
    /// Answer the step on screen
    #[effect(own_device, attended)]
    Answer {
        #[param("The value the step asks for")]
        value: String,
    },
    /// Go back a step
    #[effect(view, own_device)]
    Back,
    /// Close search
    #[effect(view, own_device, idempotent)]
    Dismiss,
}

/// The screenshot scenarios.
pub(crate) const SCENARIOS: &[&str] = &[
    "launcher",
    "results",
    "filtered",
    "app",
    "everywhere",
    "step",
    "choices",
    "more",
    "empty",
    "peek",
    "narrow",
];

/// The launcher app: the search window over [`Launcher`].
pub struct LauncherShell;

impl ResourceApp for LauncherShell {
    type Resource = Launcher;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = "Search";
    const ICON: Option<&'static str> = Some("search");
    // Nothing is kept: there is nothing to look back on.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }

    fn view(cx: &mut ViewCx<Launcher>) -> Element {
        let lent = cx.take_lent::<crate::source::LauncherService>();
        crate::surface::window(cx, lent.map(|service| service.link))
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            Some("narrow") => (420, 640),
            Some("peek") => (720, 160),
            _ => (720, 640),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_and_typing_change_only_what_the_launcher_shows() {
        let open = LauncherEffect::Open {
            scope: None,
            query: None,
        };
        assert_eq!(open.id(), "open");
        let choose = LauncherEffect::Choose {
            row: None,
            how: None,
        };
        assert_eq!(choose.id(), "choose");
    }
}

//! The launcher as a resource: the door its effects are published on, so the person, the
//! palette, an agent and a script open search, type into it, narrow it and choose what it
//! found through the same actions.

use ui::framework::{ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{self, Effect, Model, resource};
use ui::widget::Element;

/// The launcher resource: the door its effects and result rows are published on.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Launcher {}

impl Launcher {
    /// What the record holds: nothing — what is being searched is the window's own, and the
    /// keys that search are settings.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<tilo_resource::__private::serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "launcher.search",
    read_content = Launcher::read_document
)]
impl Launcher {}

/// What anyone can do with the launcher.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.launcher.search")]
pub enum LauncherEffect {
    #[effect(view, own_device, idempotent, icon = "search")]
    Open {
        #[param("Where to search: app (the app in front), workspace or everywhere")]
        scope: Option<String>,
        #[param("What to search for")]
        query: Option<String>,
    },
    #[effect(view, own_device, idempotent)]
    Search {
        #[param("What to search for")]
        query: String,
    },
    #[effect(view, own_device, idempotent)]
    Filter {
        #[param(
            "The kinds of result to show (Applications, Files, Actions…); none shows every kind"
        )]
        kinds: Vec<String>,
    },
    #[effect(view, own_device)]
    Widen,
    #[effect(view, own_device)]
    MoveSelection {
        #[param("Rows to move by; negative moves up")]
        by: i64,
    },
    #[effect(own_device, attended)]
    Choose {
        #[param("The result to choose; the selected one when left out")]
        row: Option<String>,
        #[param("How to open it: here (as a tab here), beside, or more (its other actions)")]
        how: Option<String>,
    },
    #[effect(own_device, attended)]
    Adjust {
        #[param("The result whose value to step; the selected one when left out")]
        row: Option<String>,
        #[param("1 steps up, -1 down")]
        delta: i64,
    },
    #[effect(own_device, attended)]
    Answer {
        #[param("The value the step asks for")]
        value: String,
    },
    #[effect(view, own_device)]
    Back,
    #[effect(view, own_device, idempotent)]
    Dismiss,
}

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
    const TITLE_KEY: &'static str = "launcher-app-title";
    const ICON: Option<&'static str> = Some("search");
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

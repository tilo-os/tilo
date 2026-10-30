//! The workspace overview (Super+Tab) as an ordinary app.
//!
//! It shows every workspace and pictures the strips and windows of the one
//! you are in ([`tilo_desktop::overview_panel::OverviewPanel`], the overview
//! the browser desktop draws too), and publishes what can be done from it
//! as its actions: go to a workspace or a window, move a window to another
//! workspace or strip, close one, make, join or leave a workspace, rename
//! and move a strip. The shell mounts it in its own process and lends it a
//! seat ([`OverviewSeat`]): what the desktop holds, in; what was chosen,
//! out, for the shell to carry out; and the pictures of the windows, which
//! the compositor draws where the overview names them, since their buffers
//! are other clients'. On its own (its window, a capture, a test) it shows
//! a fixed desktop ([`fixture`]).

pub mod fixture;
mod seat;
mod surface;

use ui::framework::{ResourceApp, ViewCx};
use ui::tilo_resource::{self, Effect, Model, resource};
use ui::widget::Element;

pub use seat::OverviewSeat;
pub use surface::OverviewSurface;
pub use tilo_desktop::overview_panel::{OverviewDesk, OverviewPick};

ui::embedded_catalog!(pub(crate) fn, ["en-US", "de-DE"]);

/// The app the shell mounts the overview as.
pub const APP_ID: &str = "so.tilo.overview";

/// The overview's resource: the door its effects are published on. It
/// holds nothing; what it does reaches the desktop, not a document.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Overview {}

#[resource(kind = "overview.desktop", label = "Overview")]
impl Overview {}

/// What can be done from the overview. Each acts on this device's desktop:
/// its workspaces, their strips and their windows.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "so.tilo.overview.desktop")]
pub enum OverviewEffect {
    /// Show the overview
    #[effect(
        view,
        own_device,
        idempotent,
        icon = "layout-grid",
        keywords = "workspaces, expose, mission control, all windows"
    )]
    Open,
    /// Close the overview
    #[effect(view, own_device, idempotent)]
    Close,
    /// Go to workspace
    #[effect(
        own_device,
        idempotent,
        keywords = "switch workspace, change workspace, open workspace"
    )]
    SwitchWorkspace {
        #[param("The workspace to go to, by name or id")]
        workspace: String,
    },
    /// Go to window
    #[effect(own_device, idempotent, keywords = "focus window, switch window")]
    FocusWindow {
        #[param("The window to go to, by title or id")]
        window: String,
    },
    /// Move window to workspace
    #[effect(own_device)]
    MoveWindow {
        #[param("The window to move, by title or id")]
        window: String,
        #[param("The workspace to move it to, by name or id")]
        workspace: String,
    },
    /// Move window to strip
    #[effect(own_device)]
    MoveWindowToStrip {
        #[param("The window to move, by title or id")]
        window: String,
        #[param("The strip of this workspace to move it to, by name or id")]
        strip: String,
    },
    /// Close window
    #[effect(own_device, keywords = "close tab")]
    CloseWindow {
        #[param("The window to close, by title or id")]
        window: String,
    },
    /// New workspace
    #[effect(own_device, keywords = "create workspace, add workspace")]
    CreateWorkspace,
    /// Join a workspace
    #[effect(view, own_device, keywords = "directory, community, invitation")]
    JoinWorkspace,
    /// Leave workspace
    #[effect(own_device, destructive)]
    LeaveWorkspace {
        #[param("The site's workspace to leave, by name or id")]
        workspace: String,
    },
    /// Rename strip
    #[effect(own_device, idempotent)]
    RenameStrip {
        #[param("The strip of this workspace to rename, by name or id")]
        strip: String,
        #[param("Its new name")]
        name: String,
    },
    /// Move strip
    #[effect(own_device)]
    MoveStrip {
        #[param("The strip of this workspace to move, by name or id")]
        strip: String,
        #[param("Places to move it by; negative moves it up")]
        by: i64,
    },
}

/// The screenshot scenarios.
pub(crate) const SCENARIOS: &[&str] = &[
    "overview", "keyboard", "naming", "carrying", "narrow", "page", "crowded",
];

/// The overview as an app.
pub struct OverviewApp;

impl ResourceApp for OverviewApp {
    type Resource = Overview;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Overview";
    const ICON: Option<&'static str> = Some("layout-grid");
    // Over the desktop: the overview dims what it covers itself.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is no past to look back on.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn view(cx: &mut ViewCx<Overview>) -> Element {
        let seat = cx.take_lent::<OverviewSeat>();
        let desk = cx
            .fixture::<OverviewDesk>()
            .map_or_else(fixture::desk, |desk| (*desk).clone());
        cx.surface(OverviewSurface::new(seat, desk))
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            Some("narrow") => (900, 800),
            Some("page") => (480, 640),
            _ => (1440, 900),
        }
    }

    fn fixture(fixture: &mut ui::framework::Fixture<Overview>) {
        let desk = match fixture.scenario() {
            Some("crowded") => fixture::crowded(24),
            _ => fixture::desk(),
        };
        fixture.provide(desk);
    }
}

/// The app's definition, which the shell mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(OverviewApp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn going_somewhere_and_editing_the_desktop_are_separate_doors() {
        assert_eq!(OverviewEffect::Open.id(), "open");
        let rename = OverviewEffect::RenameStrip {
            strip: "Writing".into(),
            name: "Drafts".into(),
        };
        assert_ne!(rename.id(), OverviewEffect::Close.id());
    }

    #[test]
    fn every_key_resolves_to_a_real_string() {
        for key in [
            "overview-app-title",
            "overview-app-not-renamable",
            "overview-app-bad-strip-name",
            "overview-app-strip-there",
            "overview-app-cannot-add",
            "overview-app-cannot-join",
        ] {
            assert_ne!(t(key), key, "{key} has no entry in the catalogue");
        }
        let missing = t_args("overview-app-no-window", &[("name", "Mail".into())]);
        assert!(missing.contains("Mail"), "{missing}");
    }
}

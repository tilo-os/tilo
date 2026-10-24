//! Disk Usage as a resource: scans and navigation are the
//! surface's [`DiskUsageEffect`]s. The folders analyzed lately are paths on
//! this device, so they are a device-local setting ([`RecentFolders`]),
//! published as live rows beside the resource.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ui::framework::{
    EffectAnswer, EffectCx, Effected, Fixture, LiveRow, ResourceApp, ResourceSurface, RunMode,
    SurfaceCx, ViewCx,
};
use ui::storage::{Scope, SettingsSchema};
use ui::tilo_resource::{self, Model, resource};
use ui::widget::Element;

use crate::app::DiskUsageApp;
use crate::scan::ScanResult;

/// How many analyzed folders are remembered.
const RECENT_KEPT: usize = 5;

/// The folders analyzed on this device lately, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentFolders {
    pub folders: Vec<PathBuf>,
}

impl SettingsSchema for RecentFolders {
    const SCOPE: Scope = Scope::DeviceLocal;
}

impl RecentFolders {
    /// `folder` first, then the others it was not, at most five.
    pub fn remember(&mut self, folder: PathBuf) {
        self.folders.retain(|kept| *kept != folder);
        self.folders.insert(0, folder);
        self.folders.truncate(RECENT_KEPT);
    }
}

/// What the analyzer does that changes no document: scanning a folder and
/// moving between the views.
#[derive(Clone, Debug, PartialEq, ui::framework::Effect)]
#[effect(operation = "disk-usage.effects")]
pub enum DiskUsageEffect {
    /// Analyze a folder.
    ///
    /// Scans it and shows what takes up its space.
    #[effect(job)]
    Analyze {
        #[param("The folder to analyze")]
        path: String,
    },
    /// Choose folder.
    ///
    /// Asks for a folder, then analyzes it.
    #[effect(attended, job)]
    Choose,
    /// Go back.
    ///
    /// Returns to the folder analyzed before this one, or to the locations.
    #[effect(job)]
    Back,
    /// Show locations.
    #[effect(view)]
    Home,
}

/// The disk-usage analyzer: the door its effects and live rows are
/// published on. It keeps no document state: what it scans is this device's
/// disk, and the folders it remembers are a device-local setting.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct DiskUsage {}

#[resource(kind = "disk-usage", label = "Disk Usage Analyzer")]
impl DiskUsage {}

/// One remembered folder, offered to analyze again.
pub(crate) fn recent_row(path: &std::path::Path) -> LiveRow {
    LiveRow::new(
        format!("disk-usage.recent:{}", path.display()),
        "disk-usage.recent",
        folder_name(path),
    )
    .effect(&analyze(path))
}

/// The analyze effect for `path`.
pub(crate) fn analyze(path: &std::path::Path) -> DiskUsageEffect {
    DiskUsageEffect::Analyze {
        path: path.to_string_lossy().into_owned(),
    }
}

/// A folder's name as rows and cards show it.
pub(crate) fn folder_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// One folder a live row offers to analyze.
pub(crate) fn folder_row(label: &str, path: &std::path::Path) -> LiveRow {
    LiveRow::new(
        format!("disk-usage.folder:{}", path.display()),
        "disk-usage.folder",
        label,
    )
    .effect(&analyze(path))
}

/// Framework app.
pub struct DiskUsageShell;

impl ResourceApp for DiskUsageShell {
    type Resource = DiskUsage;
    const ID: &'static str = "so.tilo.disk-usage";
    const READS_HOST: &'static [ui::launch::HostSource] = &[ui::launch::HostSource::Files];
    const TITLE: &'static str = "Disk Usage Analyzer";
    const ICON: Option<&'static str> = Some("disk-usage");

    fn view(cx: &mut ViewCx<DiskUsage>) -> Element {
        cx.surface(DiskUsageApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "analysis",
            "analysis-narrow",
            "analysis-hover",
            "analysis-hover-narrow",
            "home",
            "home-narrow",
        ]
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        if scenario.is_some_and(|name| name.ends_with("narrow")) {
            (560, 720)
        } else {
            (1180, 760)
        }
    }

    /// An analysis shows a made-up home folder instead of scanning the
    /// machine the screenshot is taken on; the home view lists two folders
    /// scanned before.
    fn fixture(fixture: &mut Fixture<DiskUsage>) {
        match fixture.scenario() {
            Some(name) if name.starts_with("home") => fixture.provide(RecentFolders {
                folders: vec![
                    PathBuf::from("/home/you/Projects/tilo"),
                    PathBuf::from(tilo_paths::WORKSPACE_MOUNT).join("design-system"),
                ],
            }),
            Some(name) if !name.starts_with("analysis") => {}
            _ => fixture.provide(ScanResult::sample()),
        }
    }
}

impl ResourceSurface<DiskUsage, DiskUsageEffect> for DiskUsageApp {
    fn present(&mut self, _state: &std::rc::Rc<DiskUsage>) {}

    /// A capture shows its scenario's folders; a live window this device's,
    /// remembering what it analyzes; a past version has none.
    fn attach(&mut self, cx: SurfaceCx<DiskUsage>) {
        self.immediate = cx.mode() == RunMode::Capture;
        self.dispatcher = Some(cx.dispatcher());
        self.actions = (cx.mode() == RunMode::Live).then(|| cx.actions());
        match cx.mode() {
            RunMode::Capture => {
                // A capture names the sample's places, never this machine's.
                self.places = crate::app::sample_places();
                if let Some(recent) = cx.fixture::<RecentFolders>() {
                    self.recent.clone_from(&recent.folders);
                }
            }
            RunMode::Live => self.recents = Some(cx.settings::<RecentFolders>()),
            RunMode::Past => {}
        }
        self.refresh_home();
        if let Some(scan) = cx.fixture::<ScanResult>() {
            self.set_result((*scan).clone());
        }
    }

    fn stage(&mut self, scenario: Option<&str>) {
        if scenario.is_some_and(|name| name.starts_with("analysis-hover")) {
            self.inspect("/home/you/projects");
        }
    }

    fn effect(
        &mut self,
        effect: DiskUsageEffect,
        cx: &mut EffectCx<DiskUsage, Self>,
    ) -> EffectAnswer {
        match effect {
            DiskUsageEffect::Analyze { path } => {
                let push_history = self.in_analysis();
                Ok(self.scan(PathBuf::from(path), push_history, cx))
            }
            DiskUsageEffect::Choose => Ok(Self::choose_folder(cx)),
            DiskUsageEffect::Back => Ok(self.go_back(cx)),
            DiskUsageEffect::Home => {
                self.go_home();
                Ok(Effected::done())
            }
        }
    }

    /// The common places, the folders analyzed lately, and the folders of
    /// the scan on screen.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.folder_rows()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use ui::prelude::TestApp;

    /// A folder of its own under the system temp directory, holding one file.
    fn scratch_folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!(
            "disk-usage-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(folder.join("photos")).unwrap();
        std::fs::write(folder.join("photos").join("beach.raw"), vec![7_u8; 4096]).unwrap();
        folder
    }

    /// The folders this device remembers.
    fn recent(app: &TestApp<DiskUsageShell>) -> Vec<PathBuf> {
        app.settings::<RecentFolders>().get().folders
    }

    /// Run frames until the scan a row started has shown and remembered
    /// `folder`.
    fn settle_until_recent(app: &mut TestApp<DiskUsageShell>, folder: &std::path::Path) {
        tilo_test_env::wait_until(format!("{} to be remembered", folder.display()), || {
            app.settle();
            recent(app).first().map(PathBuf::as_path) == Some(folder)
        });
    }

    /// Navigation, the folders on screen and the remembered folders are one
    /// published tree: the effects on the resource, the places and recents
    /// as live rows, each analyzing through the same handler.
    #[test]
    fn analyzing_a_folder_shows_it_and_remembers_it_on_this_device() {
        let folder = scratch_folder("analyze");
        let mut app = TestApp::<DiskUsageShell>::new().unwrap();
        let surface = serde_json::to_string(&app.agent_surface()).unwrap();
        for published in [
            "\"choose\"",
            "\"home\"",
            "\"back\"",
            "\"analyze\"",
            "disk-usage.folder",
        ] {
            assert!(surface.contains(published), "{published}: {surface}");
        }

        let versions = app.versions().len();
        let answer = app.effect(&analyze(&folder)).unwrap();
        let answer = answer.value.unwrap();
        assert_eq!(answer["path"], folder.to_string_lossy().as_ref());
        assert!(answer["bytes"].as_u64().unwrap() >= 4096, "{answer}");
        app.settle();
        assert_eq!(recent(&app), std::slice::from_ref(&folder));
        assert_eq!(
            app.versions().len(),
            versions,
            "a remembered folder is no edit"
        );
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("photos"), "{frame}");

        // The scanned folder's entries and the recent folder are live rows.
        // Both analyze through the same handler.
        let photos = folder.join("photos");
        let row = format!("disk-usage.folder:{}", photos.display());
        assert!(app.call_on(&row, "analyze", serde_json::json!({})).is_ok());
        settle_until_recent(&mut app, &photos);
        let recent = format!("disk-usage.recent:{}", folder.display());
        assert!(
            app.call_on(&recent, "analyze", serde_json::json!({}))
                .is_ok()
        );
        settle_until_recent(&mut app, &folder);

        // Home leaves the analysis; back with nothing analyzed before stays
        // on the locations.
        app.effect(&DiskUsageEffect::Home).unwrap();
        app.effect(&DiskUsageEffect::Back).unwrap();
        app.settle();
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("File system"), "{frame}");

        let missing = folder.join("not-there");
        assert!(app.effect(&analyze(&missing)).is_err());
        let _ = std::fs::remove_dir_all(&folder);
    }

    /// Choosing a folder asks the portal for a directory and analyzes what
    /// it answers.
    #[test]
    fn a_chosen_folder_is_analyzed() {
        let folder = scratch_folder("choose");
        let mut app = TestApp::<DiskUsageShell>::new().unwrap();
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(folder.clone()));
        let answer = ui::host::scoped(&services, || app.effect(&DiskUsageEffect::Choose)).unwrap();
        let picks = recorder.pick_requests();
        assert_eq!(picks.len(), 1);
        assert!(picks[0].is_directory());
        assert_eq!(
            answer.value.unwrap()["path"],
            folder.to_string_lossy().as_ref()
        );
        app.settle();
        assert_eq!(recent(&app), std::slice::from_ref(&folder));
        let _ = std::fs::remove_dir_all(&folder);
    }

    /// The window's bar names the place on screen and holds Choose folder
    /// and Back; the window draws no title or header row of its own.
    #[test]
    fn the_window_bar_names_the_place_and_holds_choose_folder_and_back() {
        let folder = scratch_folder("bar");
        let mut app = TestApp::<DiskUsageShell>::new().unwrap();
        app.harness().present_in_window_bar();
        app.settle();
        let bar = ui::window_toolbar::declared().expect("the shell declares the window's slots");
        assert_eq!(
            bar.location.map(|location| location.label).as_deref(),
            Some("Analyze storage")
        );
        let choose = bar
            .actions
            .iter()
            .find(|action| action.name == "Choose folder")
            .expect("Choose folder is the bar's action");
        assert!(choose.enabled && !choose.menu, "it stands in the bar's row");
        assert!(bar.leading.is_empty(), "home has no way back");
        let frame = app.harness().inspection_json().unwrap();
        assert!(!frame.contains("Analyze storage"), "{frame}");

        app.effect(&analyze(&folder)).unwrap();
        settle_until_recent(&mut app, &folder);
        let bar = ui::window_toolbar::declared().expect("the window's slots");
        let name = folder.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            bar.location.map(|location| location.label),
            Some(name),
            "the bar names the analyzed folder"
        );
        let back = bar.leading.first().expect("Back leads the bar").id.clone();
        app.harness()
            .press_window_slot(ui::window_toolbar::ToolbarEvent::Action { id: back });
        app.settle();
        let bar = ui::window_toolbar::declared().expect("the window's slots");
        assert_eq!(
            bar.location.map(|location| location.label).as_deref(),
            Some("Analyze storage"),
            "the bar's Back went home"
        );
        let _ = std::fs::remove_dir_all(&folder);
    }

    /// Every screenshot is the framework's capture: the home view lists the
    /// scenario's folders, an analysis shows the made-up scan
    /// instead of this machine's, and a narrow one gets a phone-sized window.
    #[test]
    fn captures_stage_through_fixtures() {
        use ui::prelude::TestApp;

        let mut home = TestApp::<DiskUsageShell>::capture(Some("home-narrow")).unwrap();
        assert_eq!(home.harness().inspection().viewport, [560, 720]);
        assert!(recent(&home).is_empty(), "a capture remembers nothing");
        let frame = home.harness().inspection_json().unwrap();
        assert!(frame.contains("\"tilo\""), "{frame}");

        let mut analysis = TestApp::<DiskUsageShell>::capture(Some("analysis-hover")).unwrap();
        assert_eq!(analysis.harness().inspection().viewport, [1180, 760]);
        let frame = analysis.harness().inspection_json().unwrap();
        assert!(frame.contains("Projects"), "{frame}");
        assert!(
            recent(&analysis).is_empty(),
            "a staged scan is not one this person made"
        );

        // A live window starts on the home view, with no made-up scan.
        let mut live = TestApp::<DiskUsageShell>::new().unwrap();
        let frame = live.harness().inspection_json().unwrap();
        assert!(!frame.contains("Projects"), "{frame}");
    }

    /// Folder controls use the same admitted Analyze effect as published
    /// locations, including a physical click on a remembered folder.
    #[test]
    fn folder_controls_are_certified_and_analyze_through_the_resource() {
        for scenario in ["home", "analysis"] {
            let mut capture = TestApp::<DiskUsageShell>::capture(Some(scenario)).unwrap();
            let bindings =
                ui::widget::controls::control_action_bindings(capture.harness().root_widget());
            assert!(
                bindings.iter().all(|binding| {
                    !binding.control_id.starts_with("disk-usage.place:")
                        && !binding.control_id.starts_with("disk-usage.folder-row:")
                }),
                "{scenario}: a capture must not bind unavailable device effects: {bindings:?}",
            );
            assert!(
                capture
                    .harness()
                    .controls()
                    .into_iter()
                    .any(|control| control.role == ui::accesskit::Role::ListItem),
                "{scenario}: folder rows must remain visible"
            );
            let audit = capture.harness().native_app_audit();
            assert!(audit.is_certified(), "{scenario}: {audit:?}");
        }

        let folder = scratch_folder("folder-control");
        let mut app = TestApp::<DiskUsageShell>::new().unwrap();
        let audit = app.harness().native_app_audit();
        assert!(audit.is_certified(), "live home: {audit:?}");
        let bindings = ui::widget::controls::control_action_bindings(app.harness().root_widget());
        let folders: Vec<_> = bindings
            .iter()
            .filter(|binding| binding.control_id.starts_with("disk-usage.place:"))
            .collect();
        assert!(!folders.is_empty(), "live home: no folder controls");
        for binding in folders {
            let ui::widget::ControlBindingKind::Action(address) = &binding.kind else {
                panic!("live home: folder is not action-bound: {binding:?}");
            };
            assert_eq!(address.action, "analyze");
        }
        app.settings::<RecentFolders>()
            .update(|recent| recent.remember(folder.clone()))
            .unwrap();
        app.settle();
        let id = format!("disk-usage.place:{}", folder.display());
        let target = app.harness().targets()[&id];
        app.harness().click_at(target.center().x, target.center().y);
        tilo_test_env::wait_until("the click to analyze the folder", || {
            app.settle();
            app.harness().inspection_json().unwrap().contains("photos")
        });
        let audit = app.harness().native_app_audit();
        assert!(audit.is_certified(), "analysis: {audit:?}");
        let _ = std::fs::remove_dir_all(&folder);
    }
}

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::DiskUsageShell::scenarios() {
            let mut app = TestApp::<super::DiskUsageShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}

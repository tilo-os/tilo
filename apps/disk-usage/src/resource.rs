//! Disk Usage as a [`tilo_resource::Resource`]: scans and navigation are the
//! surface's [`DiskUsageEffect`]s; recent directories are the shared
//! document. A finished scan remembers its folder with
//! [`DiskUsageAction::Remember`] through the framework dispatcher.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ui::framework::{
    Effect, EffectAnswer, EffectCx, Effected, Fixture, LiveRow, ResourceApp, ResourceSurface,
    RunMode, SurfaceCx, ViewCx,
};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ChangeKind};
use ui::tilo_resource::{self, Doc, Invoke, ModelError, Resource};
use ui::widget::Element;

use crate::app::DiskUsageApp;
use crate::doc::DiskUsageDoc;
use crate::scan::ScanResult;

/// Remember a directory the local scan just finished.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DiskUsageAction {
    Remember { path: String },
}

impl Action for DiskUsageAction {
    fn apply(&self, doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        let Self::Remember { path } = self;
        DiskUsageDoc::remember(doc, std::path::Path::new(path));
        Ok(())
    }

    /// A folder is remembered because a scan of it finished, not because
    /// anyone edited the list: no undo step forgets it.
    fn change_kind(&self) -> ChangeKind {
        ChangeKind::Housekeeping
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
    #[effect(job)]
    Choose,
    /// Go back.
    ///
    /// Returns to the folder analyzed before this one, or to the locations.
    #[effect(view, job)]
    Back,
    /// Show locations.
    #[effect(view)]
    Home,
}

/// Recent directories remembered in the shared document.
#[derive(Clone, Debug, Default)]
pub struct DiskUsageState {
    pub recent: Vec<PathBuf>,
}

/// The disk-usage analyzer resource.
pub struct DiskUsage;

impl Resource for DiskUsage {
    const KIND: &'static str = "disk-usage";
    const LABEL: &'static str = "Disk Usage Analyzer";
    const ACTION_TYPE: &'static str = "disk_usage.action";

    type State = DiskUsageState;
    type Action = DiskUsageAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<DiskUsageAction>(Self::ACTION_TYPE);
    }

    fn read(doc: &Doc) -> Result<DiskUsageState, ModelError> {
        Ok(DiskUsageState {
            recent: DiskUsageDoc::recent(doc),
        })
    }

    fn children(state: &Self::State) -> Vec<ui::tilo_actions::Resource> {
        state
            .recent
            .iter()
            .map(|path| {
                let name = folder_name(path);
                let mut resource = ui::tilo_actions::Resource::new(
                    format!("disk-usage.recent:{}", path.display()),
                    "disk-usage.recent",
                    &name,
                );
                resource
                    .actions
                    .push(analyze(path).operation(format!("Analyze {name}")));
                resource
            })
            .collect()
    }
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
    .operation(analyze(path).operation(format!("Analyze {label}")))
}

/// Framework app.
pub struct DiskUsageShell;

impl ResourceApp for DiskUsageShell {
    type Resource = DiskUsage;
    const ID: &'static str = "so.tilo.disk-usage";
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
    /// machine the screenshot is taken on.
    fn fixture(fixture: &mut Fixture<DiskUsage>) {
        if fixture
            .scenario()
            .is_none_or(|name| name.starts_with("analysis"))
        {
            fixture.provide(ScanResult::sample());
        }
    }

    /// The home view lists two folders scanned before.
    fn scenario(name: Option<&str>) -> Vec<Invoke<DiskUsage>> {
        if !name.is_some_and(|name| name.starts_with("home")) {
            return Vec::new();
        }
        [
            PathBuf::from(tilo_paths::WORKSPACE_MOUNT).join("design-system"),
            PathBuf::from("/home/you/Projects/tilo"),
        ]
        .iter()
        .filter_map(|path| {
            Invoke::apply(&DiskUsageAction::Remember {
                path: path.to_string_lossy().into_owned(),
            })
            .ok()
        })
        .collect()
    }
}

impl ResourceSurface<DiskUsage, DiskUsageEffect> for DiskUsageApp {
    /// The remembered folders. A scan on screen is this machine's and stays.
    fn present(&mut self, state: &std::rc::Rc<DiskUsageState>) {
        self.recent.clone_from(&state.recent);
    }

    fn attach(&mut self, cx: SurfaceCx<DiskUsage>) {
        self.immediate = cx.mode() == RunMode::Capture;
        self.dispatcher = Some(cx.dispatcher());
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

    /// The common places, and the folders of the scan on screen.
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

    /// Run frames until the scan a row started has shown and remembered
    /// `folder`.
    fn settle_until_recent(app: &mut TestApp<DiskUsageShell>, folder: &std::path::Path) {
        for _ in 0..600 {
            app.settle();
            if app.state().recent.first().map(PathBuf::as_path) == Some(folder) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("{} was never remembered", folder.display());
    }

    /// Navigation, the folders on screen and the remembered folders are one
    /// published tree: the effects on the resource, the places as live rows,
    /// the recents as its children, each analyzing through the same handler.
    #[test]
    fn analyzing_a_folder_shows_it_and_remembers_it_without_an_undo_step() {
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

        let answer = app.effect(&analyze(&folder)).unwrap();
        let answer = answer.value.unwrap();
        assert_eq!(answer["path"], folder.to_string_lossy().as_ref());
        assert!(answer["bytes"].as_u64().unwrap() >= 4096, "{answer}");
        app.settle();
        assert_eq!(app.state().recent, std::slice::from_ref(&folder));
        assert!(!app.undo(), "a remembered folder is no undo step");
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("photos"), "{frame}");

        // The scanned folder's entries are live rows; the recent folder is a
        // child of the resource. Both analyze through the same handler.
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
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("Choose folder"), "{frame}");

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
        assert_eq!(app.state().recent, std::slice::from_ref(&folder));
        let _ = std::fs::remove_dir_all(&folder);
    }

    /// Every screenshot is the framework's capture: the home view lists the
    /// scenario's remembered folders, an analysis shows the made-up scan
    /// instead of this machine's, and a narrow one gets a phone-sized window.
    #[test]
    fn captures_stage_through_actions_and_fixtures() {
        use ui::prelude::TestApp;

        let mut home = TestApp::<DiskUsageShell>::capture(Some("home-narrow")).unwrap();
        assert_eq!(home.harness().inspection().viewport, [560, 720]);
        assert_eq!(
            home.state().recent,
            [
                PathBuf::from("/home/you/Projects/tilo"),
                PathBuf::from(tilo_paths::WORKSPACE_MOUNT).join("design-system"),
            ]
        );
        let frame = home.harness().inspection_json().unwrap();
        assert!(frame.contains("\"tilo\""), "{frame}");

        let mut analysis = TestApp::<DiskUsageShell>::capture(Some("analysis-hover")).unwrap();
        assert_eq!(analysis.harness().inspection().viewport, [1180, 760]);
        let frame = analysis.harness().inspection_json().unwrap();
        assert!(frame.contains("Projects"), "{frame}");
        assert!(
            analysis.state().recent.is_empty(),
            "a staged scan is not one this person made"
        );

        // A live window starts on the home view, with no made-up scan.
        let mut live = TestApp::<DiskUsageShell>::new().unwrap();
        let frame = live.harness().inspection_json().unwrap();
        assert!(!frame.contains("Projects"), "{frame}");
    }
}

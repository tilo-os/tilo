//! Backups as a resource: the retention policy is document state, each
//! count a field edited at its path; where this device backs up from and to
//! is a device-local setting; backing up, verifying and restoring are the
//! surface's [`BackupsEffect`]s, and the catalog on this device is published
//! as live rows beside the resource.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Fixture, LiveRow, ResourceApp, ResourceSurface,
    SurfaceCx, ViewCx,
};
use ui::storage::{Scope, SettingsSchema};
use ui::tilo_resource::{self, Model, resource};
use ui::widget::Element;

use crate::actions::BackupsEffect;
use crate::app::BackupsApp;
use crate::model::{Catalog, RetentionPolicy};

/// What the document holds: the retention policy every device follows.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct Backups {
    pub policy: RetentionPolicy,
}

#[resource(kind = "backups.workspace", label = "Backups")]
impl Backups {}

/// The folder this device protects and the location its backups go to.
/// Absolute paths mean nothing on another device, so they never leave this
/// one; the retention policy, which does, is the document's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupLocation {
    pub source: Option<PathBuf>,
    pub destination: Option<PathBuf>,
}

impl SettingsSchema for BackupLocation {
    const SCOPE: Scope = Scope::DeviceLocal;
}

/// What a screenshot shows in place of this device's repository.
#[derive(Clone, Debug)]
pub struct DemoRepository {
    pub catalog: Catalog,
    pub location: BackupLocation,
    /// Why the saved location could not be read, for the recovery screen.
    pub error: Option<String>,
}

const SCENARIOS: &[&str] = &[
    "empty",
    "status",
    "progress",
    "failure",
    "restore",
    "sources",
    "destinations",
    "schedules",
    "unavailable",
    "rail",
];

/// Framework app.
pub struct BackupsShell;

impl ResourceApp for BackupsShell {
    type Resource = Backups;
    const ID: &'static str = "so.tilo.backups";
    const TITLE: &'static str = "Backups";
    const ICON: Option<&'static str> = Some("backups");

    fn view(cx: &mut ViewCx<Backups>) -> Element {
        cx.surface(BackupsApp::new())
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        if scenario == Some("rail") {
            (390, 844)
        } else {
            (1280, 800)
        }
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    /// Every capture but first-run onboarding (`empty`) shows the sample
    /// catalog; `unavailable` shows a saved location whose drive is gone.
    fn fixture(fixture: &mut Fixture<Backups>) {
        let demo = match fixture.scenario() {
            Some("empty") => return,
            Some("unavailable") => DemoRepository {
                catalog: Catalog::empty(),
                location: BackupLocation {
                    source: Some(PathBuf::from("/home/you/Documents")),
                    destination: Some(PathBuf::from("/media/backup-drive")),
                },
                error: Some(
                    "Couldn’t read the saved backup location: the drive is unavailable.".into(),
                ),
            },
            _ => DemoRepository {
                catalog: Catalog::sample(ui::clock::local_now().naive_local()),
                location: BackupLocation::default(),
                error: None,
            },
        };
        fixture.provide(demo);
    }
}

impl ResourceSurface<Backups, BackupsEffect> for BackupsApp {
    fn present(&mut self, state: &std::rc::Rc<Backups>) {
        self.present_policy(state.policy);
    }

    fn attach(&mut self, cx: SurfaceCx<Backups>) {
        self.attach_framework(&cx);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        if let Some(scenario) = scenario {
            self.stage_scenario(scenario);
        }
    }

    fn effect(&mut self, effect: BackupsEffect, cx: &mut EffectCx<Backups, Self>) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        crate::actions::live_rows(self.resource_catalog(), self.blocked().as_ref())
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        crate::actions::live_row(self.resource_catalog(), self.blocked().as_ref(), id)
    }

    /// Every loaded recovery point, older than the published page too.
    fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        crate::search::snapshot_entries(self.resource_catalog())
    }
}

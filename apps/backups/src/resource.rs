//! Backups' window: where this device backs up from and to, the captures it shows, and
//! what opening a search anchor does. The resource itself is `backups-model`'s.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use settings_model::{Scope, SettingsSchema};
use ui::framework::{Fixture, ResourceApp, ViewCx};
use ui::widget::Element;

use crate::actions::BackupsEffect;
use crate::model::Catalog;

pub use backups_model::Backups;

/// The folder this device protects and the location its backups go to. Absolute paths mean
/// nothing on another device, so they never leave this one.
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

pub struct BackupsShell;

impl ResourceApp for BackupsShell {
    type Resource = Backups;
    const ID: &'static str = ui::apps::BACKUPS;
    const TITLE: &'static str = "Backups";
    const TITLE_KEY: &'static str = "backups-app-title";
    const ICON: Option<&'static str> = Some("backups");

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }

    fn view(cx: &mut ViewCx<Backups>) -> Element {
        let dispatcher = cx.dispatcher();
        cx.on_open(
            move |anchor: &str| -> Result<(), ui::tilo_resource::Refusal> {
                dispatcher.effect(&opened(anchor)?);
                Ok(())
            },
        );
        crate::app::window(cx)
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

    /// Every capture but first-run onboarding (`empty`) shows the sample catalog;
    /// `unavailable` shows a saved location whose drive is gone.
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
                catalog: Catalog::sample(ui::clock::local_now().datetime()),
                location: BackupLocation::default(),
                error: None,
            },
        };
        fixture.provide(demo);
    }
}

/// What showing the search anchor `anchor` is: opening the row it names.
pub(crate) fn opened(anchor: &str) -> Result<BackupsEffect, ui::tilo_resource::Refusal> {
    anchor
        .strip_prefix("backups.snapshot:")
        .filter(|id| !id.is_empty())
        .map(|_| BackupsEffect::Open {
            row: anchor.to_owned(),
        })
        .ok_or_else(|| ui::tilo_resource::Refusal::new(format!("`{anchor}` is no backup snapshot")))
}

ui::reach_tests!(BackupsShell);

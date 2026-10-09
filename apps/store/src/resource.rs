//! App Store as a resource: report drafts, their notes and moderation
//! cases are the document ([`Reports`]); the catalogue, what is on this
//! device, the Flatpak remotes and running package jobs are live rows the
//! surface publishes; and everything the Store does to them — install,
//! open, refresh, submit a report, move around the app — is one of its
//! effects.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Fixture, LaunchInput, LiveRow, ResourceApp,
    ResourceSurface, RunMode, SurfaceCx, ViewCx,
};
use ui::tilo_actions::Resource as PublishedResource;
use ui::tilo_resource::{self, Effect, Invoke, Resource};
use ui::widget::Element;

use crate::app::StoreApp;
use crate::model::ActionKind;
use crate::reports::{Reports, report_row_id};

/// The id of Trust & moderation, the row that holds the reports.
pub const MODERATION_ID: &str = "store:moderation";

/// One app's report as the window shows it: a draft until the registry
/// knows of it, then its case.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReportProjection {
    /// The registry's case id once there is one, else `report:<app id>`.
    pub id: String,
    pub app_id: String,
    pub status: String,
    pub note: String,
}

impl ReportProjection {
    /// Whether the registry has a case for this report.
    pub fn is_case(&self) -> bool {
        self.id.starts_with("case-")
    }

    /// Every report `reports` holds, by app.
    pub fn of(reports: &Reports) -> Vec<Self> {
        reports
            .reports
            .iter()
            .map(|(app_id, report)| Self {
                id: report_row_id(app_id, report),
                app_id: app_id.clone(),
                status: report.status.clone(),
                note: report.note.clone(),
            })
            .collect()
    }
}

/// Everything the Store does that changes no document: moving around the
/// catalogue, the package jobs on this device, the Flatpak remotes, and
/// talking to a registry about a report. A package job runs on the Store's
/// worker and answers when it finished.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "store.effect", device_only)]
pub enum StoreEffect {
    /// Inspect catalogue.
    ///
    /// Read this window's current filtered page, repository scopes and worker state.
    /// An app identity requests its details without changing the selected app.
    #[effect(returns, kind = "read")]
    InspectCatalogue {
        #[param("Optional app identity whose catalogue details to inspect")]
        app: Option<String>,
    },
    /// Refresh catalog.
    #[effect(job)]
    Refresh,
    /// Update all apps.
    ///
    /// Installs the newest version of every installed app a catalogue
    /// offers an update for, the system image's apps included, one after
    /// another, without opening any.
    #[effect(job)]
    UpdateAll,
    /// Search apps.
    #[effect(kind = "read")]
    SetSearch {
        #[param("App name, publisher, category, or description")]
        query: String,
    },
    /// Browse apps from a configured Flatpak repository.
    #[effect(job, kind = "read")]
    BrowseRemote {
        #[param("Configured repository resource identity, including its account or device scope")]
        remote: String,
    },
    /// Filter apps by source.
    #[effect(kind = "read")]
    SetSource {
        #[param("All, Tilo, Flatpak, On device, or Updates")]
        source: String,
    },
    /// Filter apps by category.
    #[effect(kind = "read")]
    SetCategory {
        #[param("App category, or All")]
        category: String,
    },
    /// Clear catalogue filters.
    #[effect(kind = "read")]
    ClearFilters,
    /// Show app details.
    #[effect(kind = "read")]
    ShowDetails {
        #[param("App identity (for example so.tilo.notes)")]
        app: String,
    },
    /// Show or hide technical package details.
    #[effect(kind = "read")]
    SetTechnicalDetails {
        #[param("Whether to show package identity, digest and publisher key")]
        expanded: bool,
    },
    /// Review a local Tilo package.
    ///
    /// Opening a package installs nothing. Its identity must match a verified repository offer or an existing trusted local attestation.
    #[effect(job)]
    ImportPackage {
        #[param("Sandbox-readable .tapp path; omit to choose a file")]
        path: Option<String>,
        #[param("Handed content hash, instead of a path")]
        content: Option<String>,
        #[param("Optional handed file length; bytes are independently bounded and verified")]
        reported_size: Option<u64>,
    },
    /// Install reviewed package.
    #[effect(job)]
    InstallImportedPackage {
        #[param("Previously reviewed app identity")]
        app: String,
        #[param("Exact reviewed signed package root, without tapp: prefix")]
        root: String,
    },
    /// Open a Flatpak reference.
    ///
    /// Its app is shown ready to install; installing stays a separate step.
    #[effect(kind = "read")]
    OpenReference {
        #[param("Path of a .flatpakref file")]
        path: Option<String>,
        #[param("The text of a .flatpakref, when there is no file")]
        contents: Option<String>,
    },
    /// Back to apps.
    #[effect(kind = "read")]
    ShowCatalogue,
    /// Previous catalogue page.
    #[effect(kind = "read")]
    PreviousPage,
    /// Next catalogue page.
    #[effect(kind = "read")]
    NextPage,
    /// Show Flatpak remotes.
    #[effect(kind = "read")]
    ShowRemotes,
    /// Show trust and moderation.
    #[effect(kind = "read")]
    ShowTrust,
    /// Open.
    ///
    /// A Tilo app is fetched when it is not on this device yet.
    #[effect(job)]
    OpenApp {
        #[param("App identity")]
        app: String,
    },
    /// Update.
    #[effect(job)]
    Update {
        #[param("App identity")]
        app: String,
    },
    /// Remove.
    #[effect(job, destructive)]
    Remove {
        #[param("App identity")]
        app: String,
    },
    /// Keep on device.
    #[effect(job)]
    Pin {
        #[param("App identity")]
        app: String,
    },
    /// Allow cleanup.
    #[effect(job)]
    Unpin {
        #[param("App identity")]
        app: String,
    },
    /// Install.
    #[effect(job)]
    FlatpakInstall {
        #[param("App identity")]
        app: String,
    },
    /// Update.
    #[effect(job)]
    FlatpakUpdate {
        #[param("App identity")]
        app: String,
    },
    /// Open.
    #[effect(job)]
    FlatpakOpen {
        #[param("App identity")]
        app: String,
    },
    /// Remove.
    #[effect(job, destructive)]
    FlatpakRemove {
        #[param("App identity")]
        app: String,
    },
    /// Flatpak unavailable.
    #[effect(job)]
    FlatpakSetup {
        #[param("App identity")]
        app: String,
    },
    /// Refresh remotes.
    #[effect(job)]
    RefreshRemotes,
    /// Add Flatpak remote.
    #[effect(job)]
    AddRemote {
        #[param("Unique remote name")]
        name: String,
        #[param("HTTPS repository URL")]
        url: String,
    },
    /// Enable source.
    #[effect(job)]
    EnableRemote {
        #[param("Account repository identity")]
        remote: String,
    },
    /// Disable source.
    #[effect(job)]
    DisableRemote {
        #[param("Account repository identity")]
        remote: String,
    },
    /// Remove remote.
    #[effect(job, destructive)]
    RemoveRemote {
        #[param("Remote name")]
        name: String,
    },
    /// Submit signed report.
    ///
    /// Signs the app's report draft with your workspace identity and sends
    /// it to the app's registry.
    #[effect(job)]
    SubmitReport {
        #[param("App identity of the report")]
        app: String,
    },
    /// Refresh status.
    ///
    /// Fetches the verified status of the app's moderation case.
    #[effect(job)]
    RefreshStatus {
        #[param("App identity of the report")]
        app: String,
    },
    /// Appeal decision.
    ///
    /// Appeals the app's moderation case, with its note as the reason.
    #[effect(job)]
    Appeal {
        #[param("App identity of the report")]
        app: String,
    },
    /// Export evidence envelope.
    #[effect(job)]
    ExportEvidence {
        #[param("App identity of the report")]
        app: String,
    },
}

impl StoreEffect {
    /// The effect that carries out `action` on `app`.
    pub fn package(action: ActionKind, app: String) -> Self {
        match action {
            ActionKind::Open => Self::OpenApp { app },
            ActionKind::Update => Self::Update { app },
            ActionKind::Remove => Self::Remove { app },
            ActionKind::MarkInstalled => Self::Pin { app },
            ActionKind::Unmark => Self::Unpin { app },
            ActionKind::FlatpakInstall => Self::FlatpakInstall { app },
            ActionKind::FlatpakUpdate => Self::FlatpakUpdate { app },
            ActionKind::FlatpakOpen => Self::FlatpakOpen { app },
            ActionKind::FlatpakRemove => Self::FlatpakRemove { app },
            ActionKind::NeedsFlatpakHost => Self::FlatpakSetup { app },
        }
    }

    /// The app and the package operation this effect carries out, when it is
    /// one.
    pub fn as_package(&self) -> Option<(&str, ActionKind)> {
        let (app, action) = match self {
            Self::OpenApp { app } => (app, ActionKind::Open),
            Self::Update { app } => (app, ActionKind::Update),
            Self::Remove { app } => (app, ActionKind::Remove),
            Self::Pin { app } => (app, ActionKind::MarkInstalled),
            Self::Unpin { app } => (app, ActionKind::Unmark),
            Self::FlatpakInstall { app } => (app, ActionKind::FlatpakInstall),
            Self::FlatpakUpdate { app } => (app, ActionKind::FlatpakUpdate),
            Self::FlatpakOpen { app } => (app, ActionKind::FlatpakOpen),
            Self::FlatpakRemove { app } => (app, ActionKind::FlatpakRemove),
            Self::FlatpakSetup { app } => (app, ActionKind::NeedsFlatpakHost),
            _ => return None,
        };
        Some((app.as_str(), action))
    }
}

/// Decode the one Store link shape, `tilo-store:app/<id>`. The launcher
/// already checked the scheme; this keeps a malformed or argument-like value
/// from becoming a catalogue identity inside the app.
///
/// # Errors
/// Returns why `uri` is not a Store link to an app.
pub fn parse_store_uri(uri: &str) -> Result<String, String> {
    let Some(id) = uri.strip_prefix("tilo-store:app/") else {
        return Err("unsupported App Store link".into());
    };
    if !crate::reports::valid_app_id(id) {
        return Err("invalid App Store app identity".into());
    }
    Ok(id.to_string())
}

/// What a capture shows instead of this device's catalogue.
#[derive(Clone, Debug)]
pub struct CaptureCatalogue {
    pub items: Vec<crate::StoreItem>,
}

/// The screenshots: the wide and narrow catalogue, a search, an app's details
/// (wide and narrow, one with an update, a Flatpak app), the remotes, a
/// package job in flight and one that failed, each moderation state, and an
/// empty and an unreachable catalogue.
const SCENARIOS: &[&str] = &[
    "wide",
    "narrow",
    "search",
    "detail",
    "detail-narrow",
    "detail-technical",
    "detail-update",
    "detail-flatpak",
    "remotes",
    "account-setup",
    "account-remotes",
    "remotes-disabled",
    "search-notes",
    "paging",
    "flatpak-detail",
    "flatpak-installed",
    "progress",
    "error",
    "moderation",
    "moderation-case",
    "moderation-loading",
    "moderation-offline",
    "moderation-error",
    "empty",
    "offline",
];

/// The app the moderation screenshots report.
const SHOT_REPORTED_APP: &str = "so.tilo.notes";

/// The Store's Fluent catalogs, source locale first.
pub const LOCALES: &[(&str, &str)] = &[
    ("en-US", include_str!("../locales/en-US.ftl")),
    ("de-DE", include_str!("../locales/de-DE.ftl")),
];

pub(crate) static STORE_CATALOG: ui::i18n::EmbeddedCatalog =
    ui::i18n::EmbeddedCatalog::new("en-US", LOCALES, "store-remotes-title").with_system_locale();

/// Framework app.
pub struct StoreShell;

impl ResourceApp for StoreShell {
    type Resource = Reports;
    const ID: &'static str = "so.tilo.store";
    const TITLE: &'static str = "App Store";
    const ICON: Option<&'static str> = Some("store");
    const OPENS: &'static [&'static str] = &[
        "application/vnd.flatpak.ref",
        "application/vnd.tilo.package",
    ];
    /// A `.flatpakref` to show, or a `tilo-store:app/<id>` link.
    const ARGUMENTS: usize = 1;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&STORE_CATALOG)
    }

    fn view(cx: &mut ViewCx<Reports>) -> Element {
        cx.surface(StoreApp::new())
    }

    /// A `.flatpakref` (handed over, or its path) shows its app ready to
    /// install, and `tilo-store:app/<id>` shows that app's details. Neither
    /// installs anything: that stays the person's choice on the page.
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Reports>> {
        let effect = if let Some(file) = input.file() {
            if std::path::Path::new(file.name())
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("tapp"))
                || file.media_type() == "application/vnd.tilo.package"
            {
                let Some(content) = input.content() else {
                    return Vec::new();
                };
                // Oversized handoffs must reach the normal action refusal even
                // when the bounded content store cannot retain their bytes.
                let hash = if file.bytes().len() > crate::package_import::MAX_IMPORT_BYTES {
                    ui::framework::content::hash_bytes(file.bytes())
                } else {
                    let Ok(hash) = content.put(file.bytes()) else {
                        return Vec::new();
                    };
                    hash
                };
                return vec![Invoke::effect(&StoreEffect::ImportPackage {
                    path: None,
                    content: Some(hash),
                    reported_size: Some(file.bytes().len() as u64),
                })];
            }
            let Ok(contents) = std::str::from_utf8(file.bytes()) else {
                tracing::warn!(file = file.name(), "the opened reference is not text");
                return Vec::new();
            };
            StoreEffect::OpenReference {
                path: None,
                contents: Some(contents.to_owned()),
            }
        } else if let Some(argument) = input.argument() {
            if argument == "tilo-store:catalogue" {
                StoreEffect::ShowCatalogue
            } else if argument.starts_with("tilo-store:") {
                match parse_store_uri(argument) {
                    Ok(app) => StoreEffect::ShowDetails { app },
                    Err(error) => {
                        tracing::warn!(%error, "ignoring the App Store link");
                        return Vec::new();
                    }
                }
            } else if std::path::Path::new(argument)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("tapp"))
            {
                StoreEffect::ImportPackage {
                    path: Some(argument.to_owned()),
                    content: None,
                    reported_size: None,
                }
            } else {
                StoreEffect::OpenReference {
                    path: Some(argument.to_owned()),
                    contents: None,
                }
            }
        } else {
            return Vec::new();
        };
        vec![Invoke::effect(&effect)]
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        scenario
            .filter(|name| name.ends_with("narrow"))
            .map_or((1280, 800), |_| (520, 820))
    }

    /// A capture shows a made-up catalogue (none at all when it shows an
    /// empty or unreachable one) instead of this device's.
    fn fixture(fixture: &mut Fixture<Reports>) {
        let items = if matches!(fixture.scenario(), Some("empty" | "offline")) {
            Vec::new()
        } else {
            crate::fixture::capture_catalog(fixture.scenario())
        };
        fixture.provide(CaptureCatalogue { items });
    }

    /// The moderation screenshots show a report about Notes: a draft, or a
    /// case with its note.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Reports>> {
        let Some(name) = name.filter(|name| name.starts_with("moderation")) else {
            return Vec::new();
        };
        let app_id = SHOT_REPORTED_APP.to_owned();
        let mut actions = vec![Reports::DRAFT_REPORT.with(app_id.as_str())];
        let status = match name {
            "moderation-case" => Some("Needs information"),
            "moderation-loading" | "moderation-offline" => Some("Open"),
            "moderation-error" => Some("Quarantined"),
            _ => None,
        };
        if let Some(status) = status {
            actions.extend(Invoke::edit(&Reports::note(&app_id).replace(
                "The app contacted a third-party host while its offline mode was enabled. The exported trace contains the exact destination and time.",
            )).ok());
            actions.push(Reports::RECORD_MODERATION_CASE.with(
                crate::reports::reports::RecordModerationCase {
                    app_id,
                    case_id: "case-8d10fce04f126f0c2f293c16".into(),
                    status: status.into(),
                },
            ));
        }
        actions
    }
}

impl ResourceSurface<Reports, StoreEffect> for StoreApp {
    fn present(&mut self, state: &Rc<Reports>) {
        self.present_reports(&ReportProjection::of(state));
    }

    fn attach(&mut self, cx: SurfaceCx<Reports>) {
        self.package_content = cx.content();
        match cx.mode() {
            RunMode::Live => self.start_worker(cx.waker()),
            RunMode::Capture => {
                if let Some(catalogue) = cx.fixture::<CaptureCatalogue>() {
                    self.show_capture_catalogue(catalogue.items.clone());
                }
            }
            RunMode::Past => {}
        }
        self.attach_dispatcher(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scene(scenario);
    }

    fn effect(&mut self, effect: StoreEffect, cx: &mut EffectCx<Reports, Self>) -> EffectAnswer {
        self.answer(effect, cx)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.live_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.live_row(id)
    }

    /// Every app of the complete catalogue, not only the rows the current
    /// filter shows, each opening its details.
    fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        self.catalogue_search_entries()
    }
}

/// The resource as the palette sees it without a window: its declarations
/// and children, beside the Store's effects.
pub fn door() -> PublishedResource {
    let mut door =
        tilo_resource::publish::<Reports>(Reports::KIND, Reports::LABEL, &Reports::default());
    door.actions.extend(StoreEffect::specs());
    door
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::testing::TestApp;
    use ui::tilo_actions::OperationKind;

    /// A Store whose catalogue sources answer at once and offer nothing:
    /// no configured registry, an unreachable Flathub, a private cache.
    struct Offline {
        _env: tilo_test_env::ScopedEnv,
        dir: tempfile::TempDir,
    }

    fn offline() -> Offline {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("registries.toml"),
            "[[registry]]\nname = \"off\"\nbase_url = \"https://registry.invalid\"\nowner_pubkey_hex = \"ab\"\nenabled = false\n",
        )
        .unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_STORE_CONFIG_DIR", dir.path())
            .set("TILO_STORE_CACHE_DIR", dir.path().join("cache"))
            .set("FLATHUB_API_BASE", "http://127.0.0.1:9")
            .set("TILO_STORE_HTTP_TIMEOUT", "1");
        Offline { _env: env, dir }
    }

    fn frame(app: &mut TestApp<StoreShell>) -> String {
        app.harness().inspection_json().unwrap()
    }

    fn find(resources: &[PublishedResource], id: &str) -> Option<PublishedResource> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource.clone())
            } else {
                find(&resource.children, id)
            }
        })
    }

    /// The links a launch may name: an app's identity and nothing else.
    #[test]
    fn store_links_name_only_a_stable_app_identity() {
        assert_eq!(
            parse_store_uri("tilo-store:app/so.tilo.notes").unwrap(),
            "so.tilo.notes"
        );
        for invalid in [
            "tilo-store:app/",
            "tilo-store:app/so.tilo.notes?install=true",
            "tilo-store:remote/so.tilo.notes",
        ] {
            assert!(parse_store_uri(invalid).is_err(), "{invalid}");
        }
    }

    /// One published tree: the verbs, field edits and effects on the
    /// resource, and only effects on the live rows, a report's among them.
    #[test]
    fn the_store_publishes_one_tree() {
        let mut app = TestApp::<StoreShell>::capture(Some("moderation-case")).unwrap();
        let resources = app.harness().resources();
        let doors: Vec<_> = resources
            .iter()
            .filter(|resource| resource.kind == Reports::KIND)
            .collect();
        assert_eq!(doors.len(), 1, "one Store resource");
        assert!(
            doors[0]
                .actions
                .iter()
                .any(|action| action.id == "draft-report")
        );
        assert!(
            doors[0]
                .actions
                .iter()
                .any(|action| action.id == "field-edit"),
            "a report's note is typed through field edits"
        );
        let report = find(&resources, "case-8d10fce04f126f0c2f293c16").expect("the case");
        assert!(
            report
                .actions
                .iter()
                .all(|action| action.kind != OperationKind::Document),
            "a report's row carries its registry effects"
        );
        let apps = find(&resources, "app:so.tilo.notes").expect("the catalogue is published");
        assert!(
            apps.actions
                .iter()
                .all(|action| action.kind != OperationKind::Document),
            "a catalogue row carries only views and effects"
        );
    }

    /// Every control on screen invokes an action the published tree has: a
    /// binding the tree does not resolve withholds the window's whole action
    /// surface. Checked on the running Store (whose effects are published),
    /// on the catalogue, its remotes and its trust page.
    #[test]
    fn every_control_binds_a_published_action() {
        let _offline = offline();
        let mut app = TestApp::<StoreShell>::new().unwrap();
        app.settle();
        for (page, effect) in [
            ("catalogue", None),
            ("remotes", Some(StoreEffect::ShowRemotes)),
            ("trust", Some(StoreEffect::ShowTrust)),
        ] {
            if let Some(effect) = effect {
                app.effect(&effect).unwrap();
                app.settle();
            }
            for name in match page {
                "catalogue" => &["Refresh catalogues", "Flatpak remotes"][..],
                "remotes" => &["Back", "Refresh remotes"][..],
                _ => &[],
            } {
                let control = app
                    .harness()
                    .controls()
                    .into_iter()
                    .find(|control| control.name.as_deref() == Some(*name))
                    .unwrap_or_else(|| panic!("{page}: missing control {name}"));
                assert!(!control.disabled, "{page}: live owner control {name}");
            }
            // The production audit resolves each binding against both the
            // mounted app and the real window-owned search/History doors.
            let audit = app.harness().native_app_audit();
            let bindings =
                ui::widget::controls::control_action_bindings(app.harness().root_widget());
            assert!(!bindings.is_empty(), "{page}");
            for binding in bindings {
                if let ui::widget::ControlBindingKind::Action(address) = &binding.kind {
                    assert!(
                        !audit.issues.iter().any(|issue| {
                            issue.control == binding.control_id
                                && issue.code == "stale-action-binding"
                        }),
                        "{page}: control {} binds {address:?}, which the host cannot resolve: {:?}",
                        binding.control_id,
                        audit.issues,
                    );
                }
            }
            if page == "catalogue" {
                app.harness().click_named("search-open").unwrap();
                let targets = app.harness().targets();
                assert!(targets.contains_key("action-palette"), "{targets:?}");
                app.harness().key("Escape");
            }
        }
    }

    /// A report is drafted through its verb and its note typed at its path
    /// like someone typing: both are undo steps, and opening the Store
    /// writes nothing.
    #[test]
    fn report_notes_are_typed_into_the_document() {
        let _offline = offline();
        let mut app = TestApp::<StoreShell>::new().unwrap();
        let versions = app.versions().len();
        app.settle();
        assert_eq!(app.versions().len(), versions, "opening writes nothing");

        app.call(
            "draft-report",
            serde_json::json!({ "app_id": "so.tilo.notes" }),
        )
        .unwrap();
        app.call(
            "field-edit",
            serde_json::json!({ "edit": Reports::note("so.tilo.notes").replace("Unsigned binary") }),
        )
        .unwrap();
        let reports = app.state().reports;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports["so.tilo.notes"].note, "Unsigned binary");
        app.settle();
        let versions = app.versions().len();
        app.settle();
        assert_eq!(app.versions().len(), versions, "a report needs no repair");
        assert!(app.undo());
        assert_eq!(app.state().reports["so.tilo.notes"].note, "");
    }

    fn assert_installed_flatpak_launch(app_id: &str, id: &str) {
        let exports = tempfile::tempdir().unwrap();
        std::fs::write(
            exports.path().join(format!("{id}.desktop")),
            format!(
                concat!(
                    "[Desktop Entry]\nType=Application\nName=Installed Flatpak\n",
                    "Exec=/usr/bin/flatpak run {}\nX-Flatpak={}\n"
                ),
                id, id
            ),
        )
        .unwrap();
        let scan = desktop_entry::scan::scan_dirs(
            &[exports.path().to_path_buf()],
            &desktop_entry::scan::current_desktops(),
        );
        let desktop_id = app_id.strip_prefix("host:").unwrap();
        let entry = scan
            .find(desktop_id)
            .expect("the original launch resolves the installed Flatpak export");
        assert_eq!(entry.id, id);
        assert_eq!(
            entry.source,
            desktop_entry::EntrySource::Flatpak {
                app_id: id.to_owned()
            }
        );
    }

    #[test]
    fn declared_open_job_waits_for_its_original_channel_startup_receipt() {
        use ui::collaboration::{CollabEvent, CollabRequest};
        use ui::widget::Collaborative;
        let _offline = offline();
        let mut app = TestApp::<StoreShell>::new().unwrap();
        settle_catalogue_worker(&mut app);
        let selected = crate::fixture::fixture_catalog()
            .into_iter()
            .find(|item| {
                matches!(item.source, crate::model::StoreSource::Flathub)
                    && item.allows_action(ActionKind::FlatpakOpen, true)
            })
            .unwrap();
        let id = selected.id.clone();
        app.with_surface::<StoreApp, _>(|surface| {
            surface.set_flatpak_available(true);
            surface.set_items(vec![selected]);
        })
        .unwrap();
        app.settle();
        let row = format!("app:{id}");
        let contract = find(&app.harness().resources(), &row)
            .unwrap()
            .actions
            .into_iter()
            .find(|action| action.id == "flatpak-open")
            .unwrap()
            .contract;
        assert_eq!(contract.result, ui::tilo_actions::ResultSpec::Job);
        let invocation = app
            .start_effect_on(&row, &StoreEffect::FlatpakOpen { app: id.clone() })
            .unwrap();
        let mut request = None;
        tilo_test_env::wait_until("the original channel carries the admitted launch", || {
            app.settle();
            request = app
                .with_surface::<StoreApp, _>(|surface| {
                    surface.take_collab_requests().into_iter().next()
                })
                .flatten();
            request.is_some()
        });
        let Some(CollabRequest::LaunchInstalledApplication {
            request_id,
            app_id,
            package_root,
            on_behalf_of,
        }) = request
        else {
            panic!("the original channel must carry startup");
        };
        // Join the actual admitted launch to the host's production desktop
        // scanner, rather than duplicating its identity convention here.
        assert_installed_flatpak_launch(&app_id, &id);
        assert!(package_root.is_none());
        assert!(
            on_behalf_of.is_none(),
            "a local UI job does not invent a broker parent"
        );
        assert!(
            !app.harness()
                .job_updates(invocation)
                .iter()
                .any(ui::tilo_actions::InvocationUpdate::is_terminal)
        );
        let update = ui::tilo_actions::InvocationUpdate::Succeeded {
            invocation,
            output: ui::tilo_actions::ActionOutput::default(),
        };
        app.with_surface::<StoreApp, _>(|surface| {
            surface.handle_collab_event(&CollabEvent::PublishedInvocationResult {
                request_id,
                result: Ok((7, update.clone())),
            });
            surface.set_worker_busy(false);
            assert!(
                surface
                    .package_operation(&id, ActionKind::FlatpakOpen)
                    .is_err()
            );
        })
        .unwrap();
        assert!(
            !app.harness()
                .job_updates(invocation)
                .iter()
                .any(ui::tilo_actions::InvocationUpdate::is_terminal)
        );
        app.with_surface::<StoreApp, _>(|surface| {
            surface.handle_collab_event(&CollabEvent::InstalledApplicationLaunchResult {
                request_id,
                result: Ok((7, update)),
            });
        })
        .unwrap();
        assert_valid_launch_job_completion(&mut app, invocation, &contract);
    }

    #[test]
    fn declared_refresh_job_completes_through_the_worker_with_its_published_result_contract() {
        let _offline = offline();
        let mut app = TestApp::<StoreShell>::new().unwrap();
        settle_catalogue_worker(&mut app);
        let contract = find(&app.harness().resources(), Reports::KIND)
            .unwrap()
            .actions
            .into_iter()
            .find(|action| action.id == "refresh")
            .unwrap()
            .contract;
        assert_eq!(contract.result, ui::tilo_actions::ResultSpec::Job);
        let versions = app.versions().len();
        let invocation = app.start_effect(&StoreEffect::Refresh).unwrap();
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(5), |harness| {
                    harness
                        .job_updates(invocation)
                        .iter()
                        .any(ui::tilo_actions::InvocationUpdate::is_terminal)
                })
        );
        let updates = app.harness().job_updates(invocation);
        let output = updates
            .iter()
            .find_map(|update| match update {
                ui::tilo_actions::InvocationUpdate::Succeeded { output, .. } => Some(output),
                _ => None,
            })
            .expect("the real catalogue worker refresh succeeds");
        contract
            .result
            .validate(output)
            .expect("the worker completion honors its actual published job contract");
        assert!(output.value.is_none() && output.resources.is_empty());
        assert_eq!(
            app.with_surface::<StoreApp, _>(|surface| surface.status_line().to_owned()),
            Some("Store refreshed.".to_owned())
        );
        assert_eq!(app.versions().len(), versions, "refresh writes no document");
    }

    fn settle_catalogue_worker(app: &mut TestApp<StoreShell>) {
        tilo_test_env::wait_until("the ordinary catalogue worker settles", || {
            app.settle();
            app.with_surface::<StoreApp, _>(|surface| {
                surface.inspect_catalogue(None).unwrap()["worker_busy"] == false
            })
            .unwrap()
        });
    }

    struct CatalogueHost {
        installs: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        browses: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        registry_gate: Option<std::sync::Arc<CatalogueGate>>,
        account: bool,
        disabled: std::sync::atomic::AtomicBool,
    }

    #[derive(Default)]
    struct CatalogueGate {
        entered: std::sync::atomic::AtomicBool,
        released: std::sync::atomic::AtomicBool,
        calls: std::sync::atomic::AtomicUsize,
        canceled: std::sync::atomic::AtomicBool,
        completed: std::sync::atomic::AtomicBool,
        waker: std::sync::Mutex<Vec<std::task::Waker>>,
        registry_count: usize,
    }

    impl CatalogueGate {
        fn retire_waiter(&self) {
            let wakers = std::mem::take(&mut *self.waker.lock().unwrap());
            drop(wakers);
        }

        fn release(&self) {
            self.released
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let wakers = std::mem::take(&mut *self.waker.lock().unwrap());
            for waker in wakers {
                waker.wake();
            }
        }
    }

    struct PendingCatalogue(std::sync::Arc<CatalogueGate>);

    impl Drop for PendingCatalogue {
        fn drop(&mut self) {
            if !self.0.released.load(std::sync::atomic::Ordering::SeqCst) {
                self.0
                    .canceled
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }

    impl app_host::app_host_capnp::app_host::Server for CatalogueHost {
        fn flatpak(
            self: capnp::capability::Rc<Self>,
            params: app_host::app_host_capnp::app_host::FlatpakParams,
            mut results: app_host::app_host_capnp::app_host::FlatpakResults,
        ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
            let params = params.get().unwrap();
            assert_eq!(
                params.get_action().unwrap(),
                app_host::app_host_capnp::FlatpakAction::Install
            );
            assert_eq!(
                params.get_app_id().unwrap().to_str().unwrap(),
                "org.example.Editor"
            );
            assert!(params.get_reference().unwrap().is_empty());
            self.installs
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            results.get().set_status(
                "Installing runtime/org.example.Runtime…\nInstalling app/org.example.Editor…",
            );
            std::future::ready(Ok(()))
        }

        fn list(
            self: capnp::capability::Rc<Self>,
            _params: app_host::app_host_capnp::app_host::ListParams,
            mut results: app_host::app_host_capnp::app_host::ListResults,
        ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
            results.get().init_entries(0);
            std::future::ready(Ok(()))
        }

        fn repositories(
            self: capnp::capability::Rc<Self>,
            _params: app_host::app_host_capnp::app_host::RepositoriesParams,
            mut results: app_host::app_host_capnp::app_host::RepositoriesResults,
        ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
            let json =
                if let Some(gate) = &self.registry_gate {
                    let count = gate.registry_count.max(1);
                    let registries = (0..count).map(|index| serde_json::json!({
                    "name": format!("slow-{index}"), "base_url": "https://registry.example",
                    "owner_pubkey_hex": "11".repeat(32), "enabled": true
                })).collect::<Vec<_>>();
                    serde_json::json!({"registry": registries})
                } else {
                    serde_json::json!({"registry": []})
                };
            results.get().set_json(json.to_string());
            std::future::ready(Ok(()))
        }

        fn registry_catalogue(
            self: capnp::capability::Rc<Self>,
            _params: app_host::app_host_capnp::app_host::RegistryCatalogueParams,
            mut results: app_host::app_host_capnp::app_host::RegistryCatalogueResults,
        ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
            // Setting the result builder does not send a response: Cap'n
            // Proto waits for the actual server future to finish. Keep its
            // thread-affine builder out of the explicitly Send gate future.
            results.get().set_entries("[]");
            let gate = std::sync::Arc::clone(self.registry_gate.as_ref().unwrap());
            let call = gate.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                if call >= gate.registry_count.max(1) {
                    return Ok(());
                }
                let _pending = PendingCatalogue(std::sync::Arc::clone(&gate));
                gate.entered
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                std::future::poll_fn(|cx| {
                    let mut waker = gate.waker.lock().unwrap();
                    if gate.released.load(std::sync::atomic::Ordering::SeqCst) {
                        std::task::Poll::Ready(())
                    } else {
                        if !waker.iter().any(|waiting| waiting.will_wake(cx.waker())) {
                            waker.push(cx.waker().clone());
                        }
                        std::task::Poll::Pending
                    }
                })
                .await;
                gate.completed
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
        }

        fn flatpak_status(
            self: capnp::capability::Rc<Self>,
            _params: app_host::app_host_capnp::app_host::FlatpakStatusParams,
            mut results: app_host::app_host_capnp::app_host::FlatpakStatusResults,
        ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
            let mut result = results.get();
            result.set_available(true);
            result.set_installed(
                if self.installs.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                    "org.example.Editor\t2.0\tflathub\tuser\n"
                } else {
                    ""
                },
            );
            let row = if self.registry_gate.is_some() {
                let scope = if self.account { "user" } else { "system" };
                let disabled = if self.disabled.load(std::sync::atomic::Ordering::SeqCst) {
                    ",disabled"
                } else {
                    ""
                };
                format!("flathub\thttps://dl.flathub.org/repo/\tFlathub\t{scope}{disabled}\n")
            } else {
                "tools\thttps://tools.example/repo\tExample tools\tsystem\n".into()
            };
            result.set_remotes(&row);
            std::future::ready(Ok(()))
        }

        fn remote(
            self: capnp::capability::Rc<Self>,
            params: app_host::app_host_capnp::app_host::RemoteParams,
            mut results: app_host::app_host_capnp::app_host::RemoteResults,
        ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
            let params = params.get().unwrap();
            assert!(
                self.account,
                "the immutable device source cannot be changed"
            );
            assert_eq!(params.get_name().unwrap().to_str().unwrap(), "flathub");
            assert_eq!(
                params.get_action().unwrap(),
                app_host::app_host_capnp::RemoteAction::Disable
            );
            self.disabled
                .store(true, std::sync::atomic::Ordering::SeqCst);
            results.get().set_status("Source disabled.");
            std::future::ready(Ok(()))
        }

        fn flatpak_catalogue(
            self: capnp::capability::Rc<Self>,
            params: app_host::app_host_capnp::app_host::FlatpakCatalogueParams,
            mut results: app_host::app_host_capnp::app_host::FlatpakCatalogueResults,
        ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
            let params = params.get().unwrap();
            let name = if self.registry_gate.is_some() {
                "flathub"
            } else {
                "tools"
            };
            assert_eq!(params.get_remote().unwrap().to_str().unwrap(), name);
            assert!(
                params.get_user_managed() == self.account,
                "the original source scope reaches the host"
            );
            self.browses
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            results.get().set_rows(format!(
                "org.example.Editor\tEditor\tEdit local documents\t2.0\tstable\t{name}\n"
            ));
            std::future::ready(Ok(()))
        }
    }

    fn assert_visible_owner_control(app: &mut TestApp<StoreShell>, name: &str) {
        app.settle();
        let controls = app.harness().controls();
        let control = controls
            .iter()
            .find(|control| {
                control
                    .name
                    .as_deref()
                    .is_some_and(|label| label.starts_with(name))
                    && control.bounds.is_some()
            })
            .unwrap_or_else(|| {
                panic!("live pending-source view is missing a laid-out {name}: {controls:?}")
            });
        assert!(
            !control.disabled,
            "pending-source view must keep {name} operable"
        );
        let bounds = control
            .bounds
            .expect("the laid-out control has real geometry");
        assert!(
            bounds
                .intersect(ui::vello::kurbo::Rect::new(0.0, 0.0, 1472.0, 920.0))
                .area()
                > 0.0,
            "{name} must be visible in the real owner frame: {bounds:?}"
        );
    }

    #[test]
    fn slow_native_registry_does_not_block_configured_flatpak_browse_or_search() {
        let _offline = offline();
        let mut app = TestApp::<StoreShell>::new().unwrap();
        settle_catalogue_worker(&mut app);
        app.harness().resize((1472, 920)).unwrap();
        let gate = std::sync::Arc::new(CatalogueGate {
            registry_count: 6,
            ..CatalogueGate::default()
        });
        let browses = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (service, connection) = app_host::testing::ConnectedService::start(CatalogueHost {
            installs: std::sync::Arc::default(),
            browses: std::sync::Arc::clone(&browses),
            registry_gate: Some(std::sync::Arc::clone(&gate)),
            account: false,
            disabled: std::sync::atomic::AtomicBool::new(false),
        })
        .unwrap();
        app.with_surface::<StoreApp, _>(|surface| surface.connect_test_host(connection))
            .unwrap()
            .unwrap();
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(1), |_| {
                    gate.entered.load(std::sync::atomic::Ordering::SeqCst)
                }),
            "the actual native catalogue RPC is pending"
        );
        let remote = "remote:system:flathub";
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(1), |harness| {
                    find(&harness.resources(), remote).is_some()
                }),
            "the independent configured remote must publish while native discovery is pending"
        );
        app.effect(&StoreEffect::ShowRemotes).unwrap();
        assert_visible_owner_control(&mut app, "Flathub");
        let contract = find(&app.harness().resources(), remote)
            .unwrap()
            .actions
            .into_iter()
            .find(|action| action.id == "browse-remote")
            .unwrap()
            .contract;
        let before = browses.load(std::sync::atomic::Ordering::SeqCst);
        let invocation = app
            .start_effect_on(
                remote,
                &StoreEffect::BrowseRemote {
                    remote: remote.into(),
                },
            )
            .unwrap();
        assert_valid_unit_job_completion(&mut app, invocation, &contract);
        assert_visible_owner_control(&mut app, "Editor");
        assert_visible_owner_control(&mut app, "Search");
        assert!(browses.load(std::sync::atomic::Ordering::SeqCst) > before);
        assert!(!gate.released.load(std::sync::atomic::Ordering::SeqCst));
        app.effect(&StoreEffect::SetSearch {
            query: "Editor".into(),
        })
        .unwrap();
        tilo_test_env::wait_until(
            "the actual public Search result while native discovery is pending",
            || {
                app.settle();
                let snapshot = app
                    .effect(&StoreEffect::InspectCatalogue { app: None })
                    .unwrap()
                    .value
                    .unwrap();
                snapshot["status"]
                    .as_str()
                    .is_some_and(|status| status.contains("results for “Editor”"))
                    && snapshot["query"] == "Editor"
                    && snapshot["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|item| item["id"] == "org.example.Editor")
            },
        );
        assert!(!gate.released.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            app.with_surface::<StoreApp, _>(|surface| surface.query().to_owned()),
            Some("Editor".to_owned())
        );
        assert!(
            app.with_surface::<StoreApp, _>(|surface| surface
                .items()
                .iter()
                .any(|item| item.id == "org.example.Editor"))
                .unwrap()
        );
        assert_visible_owner_control(&mut app, "Editor");
        assert_visible_owner_control(&mut app, "Search");
        assert_background_refresh_keeps_its_locale(&mut app, remote, &contract, &gate);
        service.shutdown().unwrap();
    }

    fn assert_background_refresh_keeps_its_locale(
        app: &mut TestApp<StoreShell>,
        remote: &str,
        contract: &ui::tilo_actions::ActionContract,
        gate: &CatalogueGate,
    ) {
        let selected = ui::i18n::I18nManager::current_locale();
        let pseudo = ui::i18n::I18nManager::pseudo_locale();
        let expected = ui::i18n!("store-catalogue-refreshed").as_str().to_owned();
        ui::i18n::I18nManager::set_pseudo_locale(ui::i18n::PseudoLocale::Expanded);
        assert_ne!(ui::i18n!("store-catalogue-refreshed").as_str(), expected);
        let invocation = app
            .start_effect_on(
                remote,
                &StoreEffect::BrowseRemote {
                    remote: remote.into(),
                },
            )
            .unwrap();
        assert_valid_unit_job_completion(app, invocation, contract);
        let inspector = app
            .effect(&StoreEffect::InspectCatalogue { app: None })
            .unwrap()
            .value
            .unwrap();
        assert_eq!(inspector["worker_busy"], true);
        let calls = gate.calls.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            (2..=4).contains(&calls),
            "several native sources remain bounded and pending: {calls}"
        );
        assert!(!gate.released.load(std::sync::atomic::Ordering::SeqCst));
        gate.release();
        settle_catalogue_worker(app);
        let actual = app
            .effect(&StoreEffect::InspectCatalogue { app: None })
            .unwrap()
            .value
            .unwrap();
        ui::i18n::I18nManager::set_locale(&selected);
        ui::i18n::I18nManager::set_pseudo_locale(pseudo);
        assert_eq!(
            actual["status"], expected,
            "background refresh retains its own locale across differently localized requests"
        );
    }

    #[test]
    fn source_mutation_retires_pending_generation_without_reenabling_the_source() {
        let _offline = offline();
        let mut app = TestApp::<StoreShell>::new().unwrap();
        settle_catalogue_worker(&mut app);
        let gate = std::sync::Arc::new(CatalogueGate::default());
        let (service, connection) = app_host::testing::ConnectedService::start(CatalogueHost {
            installs: std::sync::Arc::default(),
            browses: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            registry_gate: Some(std::sync::Arc::clone(&gate)),
            account: true,
            disabled: std::sync::atomic::AtomicBool::new(false),
        })
        .unwrap();
        app.with_surface::<StoreApp, _>(|surface| surface.connect_test_host(connection))
            .unwrap()
            .unwrap();
        let remote = "remote:flathub";
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(2), |harness| {
                    gate.entered.load(std::sync::atomic::Ordering::SeqCst)
                        && find(&harness.resources(), remote).is_some()
                })
        );
        let contract = find(&app.harness().resources(), remote)
            .unwrap()
            .actions
            .into_iter()
            .find(|action| action.id == "disable-remote")
            .unwrap()
            .contract;
        let invocation = app
            .start_effect_on(
                remote,
                &StoreEffect::DisableRemote {
                    remote: remote.into(),
                },
            )
            .unwrap();
        assert_valid_unit_job_completion(&mut app, invocation, &contract);
        assert!(!gate.released.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            app.with_surface::<StoreApp, _>(|surface| surface
                .admit(StoreEffect::BrowseRemote {
                    remote: remote.into()
                })
                .is_err())
                .unwrap(),
            "the mutation readback immediately makes the disabled source unavailable"
        );
        // The old bounded server call really completes late. Its response
        // cannot overwrite the mutation's newer source state.
        gate.release();
        settle_catalogue_worker(&mut app);
        assert!(gate.completed.load(std::sync::atomic::Ordering::SeqCst));
        assert!(gate.calls.load(std::sync::atomic::Ordering::SeqCst) >= 2);
        assert!(
            app.with_surface::<StoreApp, _>(|surface| surface
                .admit(StoreEffect::BrowseRemote {
                    remote: remote.into()
                })
                .is_err())
                .unwrap(),
            "the late retired response cannot re-enable the source"
        );
        service.shutdown().unwrap();
    }

    #[test]
    fn disconnected_source_settles_refresh_and_reports_the_transport_error() {
        let _offline = offline();
        let mut app = TestApp::<StoreShell>::new().unwrap();
        settle_catalogue_worker(&mut app);
        let gate = std::sync::Arc::new(CatalogueGate::default());
        let (service, connection) = app_host::testing::ConnectedService::start(CatalogueHost {
            installs: std::sync::Arc::default(),
            browses: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            registry_gate: Some(std::sync::Arc::clone(&gate)),
            account: false,
            disabled: std::sync::atomic::AtomicBool::new(false),
        })
        .unwrap();
        app.with_surface::<StoreApp, _>(|surface| surface.connect_test_host(connection))
            .unwrap()
            .unwrap();
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(2), |_| gate
                    .entered
                    .load(std::sync::atomic::Ordering::SeqCst))
        );
        service.shutdown().unwrap();
        // The fixture owns the saved task waker too. Retire it after the
        // transport driver joins, rather than retain a task/future cycle.
        gate.retire_waiter();
        settle_catalogue_worker(&mut app);
        assert!(gate.canceled.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            app.with_surface::<StoreApp, _>(|surface| {
                surface.inspect_catalogue(None).unwrap()["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("slow"))
            })
            .unwrap(),
            "the actual source disconnect remains visible, with no fake success result"
        );
    }

    #[test]
    fn declared_browse_job_completes_through_host_rpc_worker_and_snapshot() {
        let _offline = offline();
        let mut app = TestApp::<StoreShell>::new().unwrap();
        settle_catalogue_worker(&mut app);
        let browses = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (service, connection) = app_host::testing::ConnectedService::start(CatalogueHost {
            installs: std::sync::Arc::default(),
            browses: std::sync::Arc::clone(&browses),
            registry_gate: None,
            account: false,
            disabled: std::sync::atomic::AtomicBool::new(false),
        })
        .unwrap();
        app.with_surface::<StoreApp, _>(|surface| surface.connect_test_host(connection))
            .unwrap()
            .unwrap();
        settle_catalogue_worker(&mut app);
        let remote = "remote:system:tools";
        let contract = find(&app.harness().resources(), remote)
            .unwrap()
            .actions
            .into_iter()
            .find(|action| action.id == "browse-remote")
            .unwrap()
            .contract;
        assert_eq!(contract.result, ui::tilo_actions::ResultSpec::Job);
        let before = browses.load(std::sync::atomic::Ordering::SeqCst);
        let versions = app.versions().len();
        let invocation = app
            .start_effect_on(
                remote,
                &StoreEffect::BrowseRemote {
                    remote: remote.into(),
                },
            )
            .unwrap();
        assert_valid_unit_job_completion(&mut app, invocation, &contract);
        assert_eq!(
            browses.load(std::sync::atomic::Ordering::SeqCst),
            before + 1
        );
        app.with_surface::<StoreApp, _>(|surface| {
            assert!(
                surface.items().iter().any(|item| {
                    item.id == "org.example.Editor"
                        && item.name == "Editor"
                        && item.description == "Edit local documents"
                }),
                "the real RPC catalogue reached the live snapshot"
            );
            assert!(
                surface.status_line().contains("Example tools"),
                "the worker status is a human catalogue message: {:?}",
                surface.status_line()
            );
        })
        .unwrap();
        assert_browse_uses_current_locale(&mut app, remote, &contract);
        assert_eq!(
            browses.load(std::sync::atomic::Ordering::SeqCst),
            before + 2
        );
        assert_eq!(
            app.versions().len(),
            versions,
            "Browse writes no shared document"
        );
        drop(app);
        service.shutdown().unwrap();
    }

    #[test]
    fn declared_system_flathub_install_uses_the_account_rpc_and_unit_job_receipt() {
        let _offline = offline();
        let mut app = TestApp::<StoreShell>::new().unwrap();
        settle_catalogue_worker(&mut app);
        let installs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let gate = std::sync::Arc::new(CatalogueGate::default());
        gate.release();
        let (service, connection) = app_host::testing::ConnectedService::start(CatalogueHost {
            installs: std::sync::Arc::clone(&installs),
            browses: std::sync::Arc::default(),
            registry_gate: Some(gate),
            account: false,
            disabled: std::sync::atomic::AtomicBool::new(false),
        })
        .unwrap();
        app.with_surface::<StoreApp, _>(|surface| surface.connect_test_host(connection))
            .unwrap()
            .unwrap();
        settle_catalogue_worker(&mut app);
        let remote = "remote:system:flathub";
        let resource = find(&app.harness().resources(), remote).unwrap();
        let contract = resource
            .actions
            .iter()
            .find(|action| action.id == "browse-remote")
            .unwrap()
            .contract
            .clone();
        let invocation = app
            .start_effect_on(
                remote,
                &StoreEffect::BrowseRemote {
                    remote: remote.into(),
                },
            )
            .unwrap();
        assert_valid_unit_job_completion(&mut app, invocation, &contract);
        let id = "app:org.example.Editor";
        let resource = find(&app.harness().resources(), id).unwrap();
        let contract = resource
            .actions
            .iter()
            .find(|action| action.id == "flatpak-install")
            .unwrap()
            .contract
            .clone();
        let invocation = app
            .start_effect_on(
                id,
                &StoreEffect::FlatpakInstall {
                    app: "org.example.Editor".into(),
                },
            )
            .unwrap();
        assert_valid_unit_job_completion(&mut app, invocation, &contract);
        assert_eq!(installs.load(std::sync::atomic::Ordering::SeqCst), 1);
        settle_catalogue_worker(&mut app);
        assert_eq!(
            app.with_surface::<StoreApp, _>(|surface| surface.status_line().to_owned())
                .unwrap(),
            ui::i18n!("store-app-installed")
        );
        app.with_surface::<StoreApp, _>(|surface| {
            let item = surface
                .items()
                .iter()
                .find(|item| item.id == "org.example.Editor")
                .unwrap();
            assert!(matches!(
                item.installed,
                crate::InstalledState::Installed {
                    user_managed: true,
                    ..
                }
            ));
            assert_eq!(item.action_kind(true), crate::ActionKind::FlatpakOpen);
        })
        .unwrap();
        let contract = find(&app.harness().resources(), Reports::KIND)
            .unwrap()
            .actions
            .into_iter()
            .find(|action| action.id == "refresh")
            .unwrap()
            .contract;
        let invocation = app.start_effect(&StoreEffect::Refresh).unwrap();
        assert_valid_unit_job_completion(&mut app, invocation, &contract);
        assert_eq!(
            app.with_surface::<StoreApp, _>(|surface| surface.status_line().to_owned())
                .unwrap(),
            ui::i18n!("store-catalogue-refreshed")
        );
        drop(app);
        service.shutdown().unwrap();
    }

    fn assert_valid_launch_job_completion(
        app: &mut TestApp<StoreShell>,
        invocation: ui::tilo_actions::InvocationId,
        contract: &ui::tilo_actions::ActionContract,
    ) {
        assert_valid_unit_job_completion(app, invocation, contract);
        assert_eq!(
            app.with_surface::<StoreApp, _>(|surface| surface.status_line().to_owned()),
            Some(ui::i18n!("store-launch-opened").as_str().to_owned())
        );
    }

    fn assert_valid_unit_job_completion(
        app: &mut TestApp<StoreShell>,
        invocation: ui::tilo_actions::InvocationId,
        contract: &ui::tilo_actions::ActionContract,
    ) {
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(5), |harness| {
                    harness
                        .job_updates(invocation)
                        .iter()
                        .any(ui::tilo_actions::InvocationUpdate::is_terminal)
                })
        );
        let updates = app.harness().job_updates(invocation);
        let output = updates
            .iter()
            .find_map(|update| match update {
                ui::tilo_actions::InvocationUpdate::Succeeded { output, .. } => Some(output),
                _ => None,
            })
            .expect("the declared worker job succeeds");
        contract
            .result
            .validate(output)
            .expect("the actual published job accepts its terminal output");
        assert!(output.value.is_none() && output.resources.is_empty());
    }

    fn assert_browse_uses_current_locale(
        app: &mut TestApp<StoreShell>,
        remote: &str,
        contract: &ui::tilo_actions::ActionContract,
    ) {
        let selected = ui::i18n::I18nManager::current_locale();
        let pseudo = ui::i18n::I18nManager::pseudo_locale();
        let previous = app
            .with_surface::<StoreApp, _>(|surface| surface.status_line().to_owned())
            .unwrap();
        ui::i18n::I18nManager::set_locale("fr");
        ui::i18n::I18nManager::set_pseudo_locale(ui::i18n::PseudoLocale::Expanded);
        let expected = STORE_CATALOG.t_args(
            "store-remote-results",
            &[
                ("name", ui::i18n::ArgValue::Text("Example tools".into())),
                ("count", ui::i18n::ArgValue::Text("1".into())),
            ],
        );
        assert_ne!(expected, previous, "the selected format must be observable");
        let invocation = app
            .start_effect_on(
                remote,
                &StoreEffect::BrowseRemote {
                    remote: remote.into(),
                },
            )
            .unwrap();
        assert_valid_unit_job_completion(app, invocation, contract);
        assert_eq!(
            app.with_surface::<StoreApp, _>(|surface| surface.status_line().to_owned()),
            Some(expected)
        );
        ui::i18n::I18nManager::set_locale(&selected);
        ui::i18n::I18nManager::set_pseudo_locale(pseudo);
    }

    #[test]
    fn package_import_file_handoff_refuses_unlisted_archive_without_installing() {
        let _offline = offline();
        let (bytes, _) = crate::package_import::tests::signed_fixture();
        let (file, _) =
            ui::application::OpenFile::new("signed.tapp", "application/vnd.tilo.package", bytes)
                .unwrap();
        let mut app = TestApp::<StoreShell>::handed(file).unwrap();
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(10), |harness| {
                    harness
                        .resources()
                        .iter()
                        .any(|resource| resource.id.as_str() == Reports::KIND)
                })
        );
        settle_catalogue_worker(&mut app);
        app.with_surface::<StoreApp, _>(|surface| {
            let snapshot = surface.inspect_catalogue(None).unwrap();
            assert!(snapshot["imported_package"].is_null());
            assert_eq!(snapshot["error"], ui::i18n!("store-package-offer-required"));
        })
        .unwrap();
        let root = find(&app.harness().resources(), Reports::KIND).unwrap();
        for id in ["import-package", "install-imported-package"] {
            let spec = root.actions.iter().find(|action| action.id == id).unwrap();
            assert!(spec.contract.validate_declaration().is_empty());
            assert_eq!(spec.contract.result, ui::tilo_actions::ResultSpec::Job);
        }
    }

    #[test]
    fn package_import_oversize_handoff_reports_refusal() {
        let _offline = offline();
        let (file, _) = ui::application::OpenFile::new(
            "large.tapp",
            "application/vnd.tilo.package",
            vec![0; crate::package_import::MAX_IMPORT_BYTES + 1],
        )
        .unwrap();
        let mut app = TestApp::<StoreShell>::handed(file).unwrap();
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(10), |harness| {
                    harness
                        .resources()
                        .iter()
                        .any(|resource| resource.id.as_str() == Reports::KIND)
                })
        );
        settle_catalogue_worker(&mut app);
        app.with_surface::<StoreApp, _>(|surface| {
            let snapshot = surface.inspect_catalogue(None).unwrap();
            assert!(snapshot["imported_package"].is_null());
            assert_eq!(snapshot["error"], ui::i18n!("store-package-size-refused"));
        })
        .unwrap();
    }

    /// Opening a `.flatpakref` shows its app ready to install — and
    /// installs nothing.
    #[test]
    fn launching_on_a_reference_shows_its_app() {
        let offline = offline();
        let reference = offline.dir.path().join("gimp.flatpakref");
        std::fs::write(
            &reference,
            "[Flatpak Ref]\nName=org.gimp.GIMP\nTitle=GIMP\nBranch=stable\nUrl=https://dl.flathub.org/repo/\n",
        )
        .unwrap();
        let mut app = TestApp::<StoreShell>::launched(&[reference.to_str().unwrap()]).unwrap();
        let shown = app
            .harness()
            .wait_until(ui::time::Duration::from_secs(20), |harness| {
                find(&harness.resources(), "app:org.gimp.GIMP").is_some()
            });
        assert!(shown, "the reference's app joins the catalogue");
        let row = find(&app.harness().resources(), "app:org.gimp.GIMP").unwrap();
        assert!(
            row.actions
                .iter()
                .any(|action| matches!(action.id.as_str(), "flatpak-install" | "flatpak-setup")),
            "{:?}",
            row.actions
                .iter()
                .map(|action| &action.id)
                .collect::<Vec<_>>()
        );
        assert!(frame(&mut app).contains("GIMP"));
    }

    #[test]
    fn onboarding_catalogue_link_opens_store_without_an_install() {
        let _offline = offline();
        let mut app = TestApp::<StoreShell>::launched(&["tilo-store:catalogue"]).unwrap();
        let resources = app.harness().resources();
        assert!(
            resources
                .iter()
                .any(|resource| resource.kind == Reports::KIND)
        );
        let observed = app
            .effect(&StoreEffect::InspectCatalogue { app: None })
            .unwrap()
            .value
            .unwrap();
        assert_eq!(observed.get("selected_app"), Some(&serde_json::Value::Null));
        assert_eq!(
            observed
                .get("pending_jobs")
                .and_then(serde_json::Value::as_u64),
            Some(0)
        );
        assert!(
            !frame(&mut app).contains("tilo-store:catalogue"),
            "catalogue URI must not become a package reference"
        );
        assert!(
            app.effect_progress().is_empty(),
            "opening the catalogue admits no package job"
        );
    }

    /// A `tilo-store:app/<id>` link asks for that app's details.
    #[test]
    fn launching_on_a_link_shows_that_app() {
        let _offline = offline();
        let mut app = TestApp::<StoreShell>::launched(&["tilo-store:app/so.tilo.unknown"]).unwrap();
        let asked = app
            .harness()
            .wait_until(ui::time::Duration::from_secs(20), |harness| {
                harness
                    .inspection_json()
                    .is_ok_and(|frame| frame.contains("so.tilo.unknown"))
            });
        assert!(asked, "the window asks for the linked app");
    }

    /// Every screenshot is a capture of the fixture catalogue, staged where
    /// the scenario shows, and comes out the same twice.
    #[test]
    fn captures_stage_their_scenes() {
        let mut wide = TestApp::<StoreShell>::capture(Some("wide")).unwrap();
        assert_eq!(wide.harness().inspection().viewport, [1280, 800]);
        assert!(frame(&mut wide).contains("Terminal"));
        assert!(wide.state().reports.is_empty());

        let narrow = TestApp::<StoreShell>::capture(Some("narrow")).unwrap();
        let mut narrow = narrow;
        assert_eq!(narrow.harness().inspection().viewport, [520, 820]);

        let mut detail = TestApp::<StoreShell>::capture(Some("detail")).unwrap();
        let shown = frame(&mut detail);
        assert!(shown.contains("Create report draft"), "Notes' details");
        let mut again = TestApp::<StoreShell>::capture(Some("detail")).unwrap();
        assert_eq!(frame(&mut again), shown, "a capture is deterministic");

        let mut case = TestApp::<StoreShell>::capture(Some("moderation-case")).unwrap();
        assert_eq!(
            case.state().reports[SHOT_REPORTED_APP].status,
            "Needs information"
        );
        let shown = frame(&mut case);
        assert!(shown.contains("Moderation case"));
        assert!(shown.contains("third-party host"), "the note is shown");

        let mut empty = TestApp::<StoreShell>::capture(Some("empty")).unwrap();
        assert!(!frame(&mut empty).contains("Terminal"));
    }

    /// Publisher metadata reaches the running view and the same semantic
    /// door an agent reads, without expanding ordinary catalogue listings.
    #[test]
    fn publisher_metadata_reaches_views_and_semantic_inspection() {
        let _offline = offline();
        let mut app = TestApp::<StoreShell>::new().unwrap();
        tilo_test_env::wait_until("the metadata journey's catalogue worker settles", || {
            app.settle();
            app.with_surface::<StoreApp, _>(|surface| {
                surface.inspect_catalogue(None).unwrap()["worker_busy"] == false
            })
            .unwrap()
        });
        app.with_surface(|store: &mut StoreApp| {
            store.show_capture_catalogue(crate::fixture::fixture_catalog());
            store.stage_scene(Some("detail"));
        })
        .unwrap();
        let shown = frame(&mut app);
        assert!(shown.contains("Ideas worth keeping, together."));
        assert!(shown.contains("Capture ideas, organise projects"));
        assert!(shown.contains("Supported languages"));
        assert!(shown.contains("en, es"));
        let resources = app.harness().resources();
        let notes = find(&resources, "app:so.tilo.notes").unwrap();
        assert_eq!(
            notes.presentation.unwrap().detail.as_deref(),
            Some("Ideas worth keeping, together.")
        );
        let terminal = find(&resources, "app:so.tilo.terminal").unwrap();
        assert_eq!(
            terminal.presentation.unwrap().detail.as_deref(),
            Some("A fast terminal for commands, scripts, and development tools.")
        );
        let inspection = app
            .effect(&StoreEffect::InspectCatalogue {
                app: Some("so.tilo.notes".into()),
            })
            .unwrap();
        let value = inspection.value.unwrap();
        assert_eq!(value["detail"]["tagline"], "Ideas worth keeping, together.");
        assert_eq!(
            value["detail"]["supported_languages"],
            serde_json::json!(["en", "es"])
        );
        assert_eq!(value["detail"]["supported_language_count"], 2);
        assert!(
            value["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row.get("supported_languages").is_none())
        );
    }

    #[test]
    fn captures_cover_account_setup_search_paging_and_flatpak_actions() {
        for (scenario, control) in [
            ("account-setup", "Refresh catalogues"),
            ("account-remotes", "Refresh remotes"),
        ] {
            let mut app = TestApp::<StoreShell>::capture(Some(scenario)).unwrap();
            let shown = frame(&mut app);
            assert!(!shown.contains("Use Flathub for my apps"));
            assert!(shown.contains(control), "{scenario} exposes {control}");
            if scenario == "account-remotes" {
                assert!(shown.contains("Flathub"));
            }
            assert!(app.state().reports.is_empty());
        }
        let mut search = TestApp::<StoreShell>::capture(Some("search-notes")).unwrap();
        let shown = frame(&mut search);
        assert!(shown.contains("Notes"));
        assert!(!shown.contains("Terminal"));

        let mut paging = TestApp::<StoreShell>::capture(Some("paging")).unwrap();
        let shown = frame(&mut paging);
        assert!(shown.contains("Page 2 of 2"));
        assert!(shown.contains("Capture app 48"));
        assert!(!shown.contains("Capture app 00"));

        for (scenario, name, action) in [
            ("flatpak-detail", "Chess", "Install"),
            ("flatpak-installed", "VLC", "Open"),
        ] {
            let mut app = TestApp::<StoreShell>::capture(Some(scenario)).unwrap();
            let shown = frame(&mut app);
            assert!(shown.contains(name));
            assert!(shown.contains(action));
        }
    }
}

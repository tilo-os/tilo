//! App Store as a [`tilo_resource::Resource`]: report drafts, their notes and
//! moderation cases are the document; the catalogue, what is on this device,
//! the Flatpak remotes and running package jobs are live rows the surface
//! publishes; and everything the Store does to them — install, open, refresh,
//! submit a report, move around the app — is one of its effects.

use std::rc::Rc;

use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectStatus, Fixture, LaunchInput, LiveRow, ResourceApp,
    ResourceSurface, RunMode, SurfaceCx, ViewCx,
};
use ui::tilo_actions::{ActionParam, ParamType, Resource as PublishedResource};
use ui::tilo_resource::{
    self, ActionDecl, Doc, Effect, Invoke, ModelError, PlainText, Resource, SearchEntry, TextEdit,
    child_resource_id, publish_child,
};
use ui::widget::Element;

use crate::app::StoreApp;
use crate::doc_actions::{
    STORE_DOCUMENT_ACTION_TYPE, StoreDocumentAction, register, state_from_doc,
};
use crate::model::ActionKind;

/// One app's report: a draft until the registry knows of it, then its case.
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
}

/// What the Store document holds: collaborative reports. Installed apps,
/// remotes and download progress are this device's, not the document's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreState {
    pub reports: Vec<ReportProjection>,
}

/// The app-store catalogue resource.
pub struct Store;

impl Resource for Store {
    const KIND: &'static str = "store.catalog";
    const LABEL: &'static str = "App Store";
    const ACTION_TYPE: &'static str = STORE_DOCUMENT_ACTION_TYPE;

    type State = StoreState;
    type Action = StoreDocumentAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<StoreState, ModelError> {
        Ok(state_from_doc(doc))
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "draft-report",
                "Create report draft",
                vec![ActionParam::required(
                    "app_id",
                    ParamType::Text,
                    "App identity (for example so.tilo.notes)",
                )],
                |args, _| {
                    let app_id = args
                        .text("app_id")
                        .ok_or_else(|| "missing app id".to_owned())?
                        .trim()
                        .to_owned();
                    if app_id.is_empty() {
                        return Err("missing app id".into());
                    }
                    Ok(StoreDocumentAction::CreateReportDraft { app_id })
                },
            )
            .describe("Start a collaborative report about an app, with a note to describe it."),
        ]
    }

    /// Typing one word into a report note is one step.
    fn coalesces(previous: &StoreDocumentAction, next: &StoreDocumentAction) -> bool {
        match (previous, next) {
            (
                StoreDocumentAction::EditReportNote {
                    app_id: before,
                    edit: previous,
                },
                StoreDocumentAction::EditReportNote { app_id, edit },
            ) => before == app_id && PlainText::coalesces(previous, edit),
            _ => false,
        }
    }

    /// Trust & moderation, holding each report with its note as a plain
    /// text of its own, so the palette, agents and scripts write a note like
    /// someone typing it.
    fn children(state: &StoreState) -> Vec<PublishedResource> {
        let mut moderation =
            PublishedResource::new(MODERATION_ID, "store.moderation", "Trust & moderation");
        for report in &state.reports {
            let mut resource = PublishedResource::new(
                report.id.clone(),
                "store.report",
                format!("{} · {}", report.app_id, report.status),
            );
            let app_id = report.app_id.clone();
            resource.children.push(publish_child::<PlainText, Self>(
                &report_note_id(&report.app_id),
                &format!("Concern about {}", report.app_id),
                Some(&report.note),
                move |edit| StoreDocumentAction::EditReportNote {
                    app_id: app_id.clone(),
                    edit,
                },
            ));
            moderation.children.push(resource);
        }
        vec![moderation]
    }

    fn search(state: &StoreState) -> Vec<SearchEntry> {
        state
            .reports
            .iter()
            .map(|report| {
                SearchEntry::new(format!("Report · {}", report.app_id), report.status.clone())
                    .anchored(format!("store.report:{}", report.app_id))
            })
            .collect()
    }
}

/// The id of Trust & moderation, which holds the reports.
pub const MODERATION_ID: &str = "store:moderation";

/// The id the note of `app_id`'s report is published under.
pub fn report_note_id(app_id: &str) -> String {
    child_resource_id::<Store>(&format!("report-note:{app_id}"))
}

/// Edits of the note of `app_id`'s report through the resource's dispatcher.
pub fn report_note_dispatcher(
    dispatcher: &Dispatcher<Store>,
    app_id: &str,
) -> Dispatcher<PlainText> {
    let app_id = app_id.to_owned();
    dispatcher.child::<PlainText>(&report_note_id(&app_id), move |edit| {
        StoreDocumentAction::EditReportNote {
            app_id: app_id.clone(),
            edit,
        }
    })
}

/// Everything the Store does that changes no document: moving around the
/// catalogue, the package jobs on this device, the Flatpak remotes, and
/// talking to a registry about a report. A package job runs on the Store's
/// worker and answers when it finished.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "store.effect")]
pub enum StoreEffect {
    /// Refresh catalog.
    #[effect(job)]
    Refresh,
    /// Search apps.
    #[effect(permission = "read")]
    SetSearch {
        #[param("App name, publisher, category, or description")]
        query: String,
    },
    /// Filter apps by source.
    #[effect(permission = "read")]
    SetSource {
        #[param("All, Tilo, Flatpak, or On device")]
        source: String,
    },
    /// Filter apps by category.
    #[effect(permission = "read")]
    SetCategory {
        #[param("App category, or All")]
        category: String,
    },
    /// Clear catalogue filters.
    #[effect(permission = "read")]
    ClearFilters,
    /// Show app details.
    #[effect(permission = "read")]
    ShowDetails {
        #[param("App identity (for example so.tilo.notes)")]
        app: String,
    },
    /// Open a Flatpak reference.
    ///
    /// Its app is shown ready to install; installing stays a separate step.
    #[effect(permission = "read")]
    OpenReference {
        #[param("Path of a .flatpakref file")]
        path: Option<String>,
        #[param("The text of a .flatpakref, when there is no file")]
        contents: Option<String>,
    },
    /// Back to apps.
    #[effect(permission = "read")]
    ShowCatalogue,
    /// Previous catalogue page.
    #[effect(permission = "read")]
    PreviousPage,
    /// Next catalogue page.
    #[effect(permission = "read")]
    NextPage,
    /// Show Flatpak remotes.
    #[effect(permission = "read")]
    ShowRemotes,
    /// Show trust and moderation.
    #[effect(permission = "read")]
    ShowTrust,
    /// Open.
    ///
    /// A Tilo app is fetched when it is not on this device yet.
    #[effect(job)]
    Open {
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
    /// Install flatpak first.
    #[effect(job)]
    FlatpakSetup {
        #[param("App identity")]
        app: String,
    },
    /// Refresh remotes.
    #[effect(job)]
    RefreshRemotes,
    /// Add Flatpak remote.
    #[effect(job, permission = "admin")]
    AddRemote {
        #[param("Unique remote name")]
        name: String,
        #[param("HTTPS repository URL")]
        url: String,
    },
    /// Remove remote.
    #[effect(job, destructive, permission = "admin")]
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
            ActionKind::Open => Self::Open { app },
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
            Self::Open { app } => (app, ActionKind::Open),
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
    if !crate::doc_actions::valid_app_id(id) {
        return Err("invalid App Store app identity".into());
    }
    Ok(id.to_string())
}

/// What a capture shows instead of this device's catalogue.
#[derive(Clone, Debug)]
pub struct CaptureCatalogue {
    pub items: Vec<crate::StoreItem>,
}

/// The screenshots: the wide and narrow catalogue, an app's details, the
/// remotes, a package job in flight and one that failed, each moderation
/// state, and an empty and an unreachable catalogue.
const SCENARIOS: &[&str] = &[
    "wide",
    "narrow",
    "detail",
    "remotes",
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

/// Framework app.
pub struct StoreShell;

impl ResourceApp for StoreShell {
    type Resource = Store;
    const ID: &'static str = "so.tilo.store";
    const TITLE: &'static str = "App Store";
    const ICON: Option<&'static str> = Some("store");
    const OPENS: &'static [&'static str] = &["application/vnd.flatpak.ref"];
    /// A `.flatpakref` to show, or a `tilo-store:app/<id>` link.
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Store>) -> Element {
        cx.surface(StoreApp::new())
    }

    /// A `.flatpakref` (handed over, or its path) shows its app ready to
    /// install, and `tilo-store:app/<id>` shows that app's details. Neither
    /// installs anything: that stays the person's choice on the page.
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Store>> {
        let effect = if let Some(file) = input.file() {
            let Ok(contents) = std::str::from_utf8(file.bytes()) else {
                tracing::warn!(file = file.name(), "the opened reference is not text");
                return Vec::new();
            };
            StoreEffect::OpenReference {
                path: None,
                contents: Some(contents.to_owned()),
            }
        } else if let Some(argument) = input.argument() {
            if argument.starts_with("tilo-store:") {
                match parse_store_uri(argument) {
                    Ok(app) => StoreEffect::ShowDetails { app },
                    Err(error) => {
                        tracing::warn!(%error, "ignoring the App Store link");
                        return Vec::new();
                    }
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
        match scenario {
            Some("narrow") => (420, 860),
            _ => (1200, 760),
        }
    }

    /// A capture shows a made-up catalogue (none at all when it shows an
    /// empty or unreachable one) instead of this device's.
    fn fixture(fixture: &mut Fixture<Store>) {
        let items = if matches!(fixture.scenario(), Some("empty" | "offline")) {
            Vec::new()
        } else {
            crate::fixture::fixture_catalog()
        };
        fixture.provide(CaptureCatalogue { items });
    }

    /// The moderation screenshots show a report about Notes: a draft, or a
    /// case with its note.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Store>> {
        let Some(name) = name.filter(|name| name.starts_with("moderation")) else {
            return Vec::new();
        };
        let app_id = SHOT_REPORTED_APP.to_owned();
        let mut actions = vec![StoreDocumentAction::CreateReportDraft {
            app_id: app_id.clone(),
        }];
        let status = match name {
            "moderation-case" => Some("Needs information"),
            "moderation-loading" => Some("Open"),
            "moderation-offline" => Some("Last confirmed · Open"),
            "moderation-error" => Some("Last confirmed · Quarantined"),
            _ => None,
        };
        if let Some(status) = status {
            actions.push(StoreDocumentAction::EditReportNote {
                app_id: app_id.clone(),
                edit: TextEdit::ReplaceText {
                    text: "The app contacted a third-party host while its offline mode was enabled. The exported trace contains the exact destination and time.".into(),
                },
            });
            actions.push(StoreDocumentAction::RecordModerationCase {
                app_id,
                case_id: "case-8d10fce04f126f0c2f293c16".into(),
                status: status.into(),
            });
        }
        actions
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
    }
}

impl ResourceSurface<Store, StoreEffect> for StoreApp {
    fn present(&mut self, state: &Rc<StoreState>) {
        self.present_reports(&state.reports);
    }

    fn attach(&mut self, cx: SurfaceCx<Store>) {
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

    fn effect(&mut self, effect: StoreEffect, cx: &mut EffectCx<Store, Self>) -> EffectAnswer {
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
}

/// The resource as the palette sees it without a window: its declarations
/// and children, beside the Store's effects.
pub fn door() -> PublishedResource {
    let mut door =
        tilo_resource::publish::<Store>(Store::KIND, Store::LABEL, &StoreState::default());
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

    /// One published tree: the declarations and effects on the resource,
    /// reports with their notes as children, and only effects on the live
    /// rows.
    #[test]
    fn the_store_publishes_one_tree() {
        let mut app = TestApp::<StoreShell>::capture(Some("moderation-case")).unwrap();
        let resources = app.harness().resources();
        let doors: Vec<_> = resources
            .iter()
            .filter(|resource| resource.kind == Store::KIND)
            .collect();
        assert_eq!(doors.len(), 1, "one Store resource");
        assert!(
            doors[0]
                .actions
                .iter()
                .any(|action| action.id == "draft-report")
        );
        let report = find(&resources, "case-8d10fce04f126f0c2f293c16").expect("the case");
        assert!(
            report
                .children
                .iter()
                .any(|child| child.id.as_str() == report_note_id(SHOT_REPORTED_APP)),
            "the note is a child of its report"
        );
        let apps = find(&resources, "app:so.tilo.notes").expect("the catalogue is published");
        assert!(
            apps.actions
                .iter()
                .all(|action| action.kind != OperationKind::Document),
            "a catalogue row carries only views and effects"
        );
    }

    /// A report is drafted through the declared action and its note typed
    /// through its child like someone typing: both are undo steps, and
    /// opening the Store writes nothing.
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
        app.invoke_child::<PlainText>(
            &report_note_id("so.tilo.notes"),
            Invoke::<PlainText>::apply(&TextEdit::ReplaceText {
                text: "Unsigned binary".into(),
            })
            .unwrap(),
        )
        .unwrap();
        let reports = app.state().reports;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].note, "Unsigned binary");
        app.settle();
        let versions = app.versions().len();
        app.settle();
        assert_eq!(app.versions().len(), versions, "a report needs no repair");
        assert!(app.undo());
        assert_eq!(app.state().reports[0].note, "");
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
        assert_eq!(wide.harness().inspection().viewport, [1200, 760]);
        assert!(frame(&mut wide).contains("Terminal"));
        assert!(wide.state().reports.is_empty());

        let narrow = TestApp::<StoreShell>::capture(Some("narrow")).unwrap();
        let mut narrow = narrow;
        assert_eq!(narrow.harness().inspection().viewport, [420, 860]);

        let mut detail = TestApp::<StoreShell>::capture(Some("detail")).unwrap();
        let shown = frame(&mut detail);
        assert!(shown.contains("Create report draft"), "Notes' details");
        let mut again = TestApp::<StoreShell>::capture(Some("detail")).unwrap();
        assert_eq!(frame(&mut again), shown, "a capture is deterministic");

        let mut case = TestApp::<StoreShell>::capture(Some("moderation-case")).unwrap();
        assert_eq!(case.state().reports[0].status, "Needs information");
        let shown = frame(&mut case);
        assert!(shown.contains("Moderation case"));
        assert!(shown.contains("third-party host"), "the note is shown");

        let mut empty = TestApp::<StoreShell>::capture(Some("empty")).unwrap();
        assert!(!frame(&mut empty).contains("Terminal"));
    }
}

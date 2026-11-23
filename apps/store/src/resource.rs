//! App Store as a resource: report drafts, their notes and moderation
//! cases are the document ([`Reports`]); the catalogue, what is on this
//! device, the Flatpak remotes and running package jobs are live rows the
//! view publishes; and everything the Store does to them — install,
//! open, refresh, submit a report, move around the app — is one of its
//! effects.

use ui::framework::{Fixture, LaunchInput, ResourceApp, ViewCx};
use ui::tilo_actions::ResourceSpec as PublishedResource;
use ui::tilo_resource::{self, AbsolutePath, Effect, Invoke, Resource};
use ui::widget::Element;

use crate::app::StoreApp;
use crate::model::ActionKind;
use crate::reports::{Reports, report_row_id};

pub const MODERATION_ID: &str = "store:moderation";

/// One app of the catalogue, as published beneath the Store: its versions and
/// review hang beneath it, and it offers what can be done with it now.
#[derive(tilo_resource::Row)]
#[row(kind = "store.app", label = "App", effects = StoreEffect)]
pub struct CatalogueApp;

/// One version of a catalogue app.
#[derive(tilo_resource::Row)]
#[row(kind = "store.version", label = "Version")]
pub struct StoreVersion;

/// What the Store's trust of one app comes to.
#[derive(tilo_resource::Row)]
#[row(kind = "store.review", label = "Review")]
pub struct StoreReview;

/// One publisher's apps, gathered.
#[derive(tilo_resource::Row)]
#[row(kind = "store.publisher", label = "Publisher")]
pub struct StorePublisher;

/// The rest of a catalogue too long to publish, found through search.
#[derive(tilo_resource::Row)]
#[row(kind = "store.catalogue-snapshot", label = "Catalogue snapshot")]
pub struct CatalogueSnapshot;

/// The Flatpak remotes this device offers, as one row.
#[derive(tilo_resource::Row)]
#[row(kind = "store.remote-list", label = "Flatpak remotes", effects = StoreEffect)]
pub struct RemoteList;

/// One Flatpak remote.
#[derive(tilo_resource::Row)]
#[row(kind = "store.remote", label = "Flatpak remote", effects = StoreEffect)]
pub struct StoreRemote;

/// Trust and moderation, as one row.
#[derive(tilo_resource::Row)]
#[row(kind = "store.moderation", label = "Trust and moderation", effects = StoreEffect)]
pub struct Moderation;

/// One report or moderation case.
#[derive(tilo_resource::Row)]
#[row(kind = "store.report", label = "Report", effects = StoreEffect)]
pub struct ReportRow;

/// The package operation running now.
#[derive(tilo_resource::Row)]
#[row(kind = "store.install-job", label = "Package job")]
pub struct InstallJob;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReportProjection {
    /// The registry's case id once there is one, else `report:<app id>`.
    pub id: String,
    pub app_id: String,
    pub status: String,
    pub note: String,
}

impl ReportProjection {
    pub fn is_case(&self) -> bool {
        self.id.starts_with("case-")
    }

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

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "store.effect")]
pub enum StoreEffect {
    #[effect(returns, kind = "read")]
    InspectCatalogue {
        #[param("Optional app identity whose catalogue details to inspect")]
        app: Option<String>,
    },
    #[effect(kind = "outside")]
    #[effect(job)]
    Refresh,
    #[effect(kind = "outside")]
    #[effect(job)]
    UpdateAll,
    /// Stop the change this window started: the workspace cancels the invocation and this
    /// device stops waiting for it.
    #[effect(kind = "outside")]
    #[effect(idempotent)]
    #[effect(label = "store-progress-cancel")]
    CancelChange,
    #[effect(screen)]
    SetSearch {
        #[param("App name, publisher, category, or description")]
        query: String,
    },
    #[effect(screen)]
    ShowView {
        #[param("home, updates, installed or bots")]
        view: String,
    },
    #[effect(screen)]
    ShowMore,
    /// Block app.
    ///
    /// The app then appears nowhere in the Store or in search, and cannot be
    /// opened or installed from them. Only you can unblock it.
    #[effect(kind = "outside")]
    #[effect(idempotent)]
    #[effect(label = "store-block-button")]
    BlockApp {
        #[param("App identity (for example com.example.Game)")]
        app: String,
    },
    #[effect(kind = "outside")]
    #[effect(idempotent)]
    UnblockApp {
        #[param("App identity")]
        app: String,
    },
    #[effect(job, kind = "read", on = StoreRemote)]
    BrowseRemote {
        #[param("Configured repository resource identity, including its account or device scope")]
        remote: String,
    },
    #[effect(screen)]
    SetCategory {
        #[param("App category, or All")]
        category: String,
    },
    #[effect(screen)]
    ClearFilters,
    #[effect(screen, on = CatalogueApp)]
    ShowDetails {
        #[param("App identity (for example so.tilo.notes)")]
        app: String,
    },
    #[effect(job, kind = "read")]
    ShowRuntimeDetails {
        #[param("Exact runtime reference, for example tilo-runtime/2026.10.4-chromium")]
        runtime: String,
        #[param("Optional enabled repository name; omit only when its offer is unambiguous")]
        registry: Option<String>,
    },
    #[effect(kind = "outside", on = CatalogueApp)]
    #[effect(job)]
    InstallRuntime {
        #[param("Reviewed runtime package identity")]
        app: String,
        #[param("Exact reviewed signed package root")]
        root: String,
    },
    #[effect(screen)]
    SetTechnicalDetails {
        #[param("Whether to show package identity, digest and publisher key")]
        expanded: bool,
    },
    /// Review a local Tilo package.
    ///
    /// Opening a package installs nothing. Its identity must match a verified repository offer or an existing trusted local attestation.
    #[effect(kind = "outside")]
    #[effect(job)]
    ImportPackage {
        #[param("Absolute, sandbox-readable .tapp path; omit to choose a file")]
        path: Option<AbsolutePath>,
        #[param("Handed content hash, instead of a path")]
        content: Option<String>,
        #[param("Optional handed file length; bytes are independently bounded and verified")]
        reported_size: Option<u64>,
    },
    #[effect(kind = "outside")]
    #[effect(job)]
    #[effect(label = "store-package-install")]
    InstallImportedPackage {
        #[param("Previously reviewed app identity")]
        app: String,
        #[param("Exact reviewed signed package root, without tapp: prefix")]
        root: String,
    },
    #[effect(screen)]
    OpenReference {
        #[param("Absolute path of a .flatpakref file")]
        path: Option<AbsolutePath>,
        #[param("The text of a .flatpakref, when there is no file")]
        contents: Option<String>,
    },
    #[effect(screen)]
    ShowCatalogue,
    #[effect(screen, on = RemoteList)]
    ShowRemotes,
    #[effect(screen, on = Moderation)]
    ShowTrust,
    #[effect(kind = "outside")]
    #[effect(idempotent)]
    OpenLink {
        #[param("Web address (http or https)")]
        url: String,
    },
    /// Get.
    ///
    /// Add the newest version the app's source offers to this workspace. Every device of the
    /// workspace then fetches it, and the app opens here once it has arrived.
    #[effect(kind = "outside", on = CatalogueApp)]
    #[effect(job)]
    #[effect(label = "store-action-get")]
    GetApp {
        #[param("App identity")]
        app: String,
    },
    /// Open.
    ///
    /// The app opens from this device once it has the version the workspace runs; a press
    /// before then waits for it rather than being dropped.
    #[effect(kind = "outside", on = CatalogueApp)]
    #[effect(job, returns)]
    #[effect(label = "store-action-open")]
    OpenApp {
        #[param("App identity")]
        app: String,
    },
    #[effect(kind = "outside", on = CatalogueApp)]
    #[effect(job)]
    #[effect(label = "store-action-update")]
    Update {
        #[param("App identity")]
        app: String,
    },
    /// Uninstall.
    ///
    /// Stop running the version in this workspace. This device keeps or removes its copy by its
    /// own rules once no workspace names it.
    #[effect(kind = "outside", on = CatalogueApp)]
    #[effect(job)]
    #[effect(label = "store-action-remove")]
    UninstallApp {
        #[param("App identity")]
        app: String,
    },
    #[effect(kind = "outside", on = CatalogueApp)]
    #[effect(job)]
    Pin {
        #[param("App identity")]
        app: String,
    },
    #[effect(kind = "outside", on = CatalogueApp)]
    #[effect(job)]
    #[effect(label = "store-action-release")]
    Unpin {
        #[param("App identity")]
        app: String,
    },
    #[effect(kind = "outside", on = CatalogueApp)]
    #[effect(job)]
    #[effect(label = "store-action-install")]
    FlatpakInstall {
        #[param("App identity")]
        app: String,
    },
    #[effect(kind = "outside", on = CatalogueApp)]
    #[effect(job)]
    #[effect(label = "store-action-update")]
    FlatpakUpdate {
        #[param("App identity")]
        app: String,
    },
    #[effect(kind = "outside", on = CatalogueApp)]
    #[effect(job)]
    #[effect(label = "store-action-open")]
    FlatpakOpen {
        #[param("App identity")]
        app: String,
    },
    #[effect(kind = "outside", on = CatalogueApp)]
    #[effect(job)]
    #[effect(label = "store-flatpak-unavailable")]
    FlatpakSetup {
        #[param("App identity")]
        app: String,
    },
    /// Add a directory bot to a workspace.
    ///
    /// Bots run in workspaces, not on this device; this stays unavailable
    /// until a workspace offers its install-bot action to you.
    #[effect(kind = "outside", on = CatalogueApp)]
    #[effect(job)]
    AddBot {
        #[param("Bot identity")]
        app: String,
    },
    #[effect(kind = "outside", on = RemoteList)]
    #[effect(job)]
    RefreshRemotes,
    /// Add Flatpak remote.
    #[effect(kind = "outside")]
    #[effect(job)]
    AddRemote {
        #[param("Unique remote name")]
        name: String,
        #[param("HTTPS repository URL")]
        url: String,
    },
    #[effect(kind = "outside", on = StoreRemote)]
    #[effect(job)]
    #[effect(label = "store-remote-enable")]
    EnableRemote {
        #[param("Account repository identity")]
        remote: String,
    },
    #[effect(kind = "outside", on = StoreRemote)]
    #[effect(job)]
    #[effect(label = "store-remote-disable")]
    DisableRemote {
        #[param("Account repository identity")]
        remote: String,
    },
    /// Remove remote.
    #[effect(kind = "outside", on = StoreRemote)]
    #[effect(job, destructive)]
    RemoveRemote {
        #[param("Remote name")]
        name: String,
    },
    #[effect(returns, kind = "read")]
    PrepareModeration {
        #[param("App identity of the report")]
        app: String,
        #[param("Prepare an appeal instead of a report")]
        appeal: bool,
    },
    #[effect(kind = "outside", on = ReportRow)]
    #[effect(job, sensitive)]
    SubmitReport {
        #[param("App identity of the report")]
        app: String,
        #[param("Exact operation JSON returned by prepare-moderation")]
        operation: String,
    },
    #[effect(kind = "outside", on = ReportRow)]
    #[effect(job)]
    #[effect(label = "store-report-refresh")]
    RefreshStatus {
        #[param("App identity of the report")]
        app: String,
    },
    #[effect(kind = "outside", on = ReportRow)]
    #[effect(job, sensitive)]
    #[effect(label = "store-report-appeal")]
    Appeal {
        #[param("App identity of the report")]
        app: String,
        #[param("Exact operation JSON returned by prepare-moderation")]
        operation: String,
    },
    #[effect(kind = "outside", on = ReportRow)]
    #[effect(job)]
    #[effect(label = "store-report-export")]
    ExportEvidence {
        #[param("App identity of the report")]
        app: String,
    },
}

impl StoreEffect {
    pub fn package(action: ActionKind, item: &crate::StoreItem) -> Option<Self> {
        let app = item.id.clone();
        Some(match action {
            ActionKind::RuntimeInstall => Self::InstallRuntime {
                app,
                root: item
                    .versions
                    .first()
                    .and_then(|version| version.root_hash_hex.clone())
                    .unwrap_or_default(),
            },
            ActionKind::Get => Self::GetApp { app },
            ActionKind::Open => Self::OpenApp { app },
            ActionKind::Update => Self::Update { app },
            ActionKind::Remove => Self::UninstallApp { app },
            ActionKind::FlatpakRemove => return None,
            ActionKind::MarkInstalled => Self::Pin { app },
            ActionKind::Unmark => Self::Unpin { app },
            ActionKind::FlatpakInstall => Self::FlatpakInstall { app },
            ActionKind::FlatpakUpdate => Self::FlatpakUpdate { app },
            ActionKind::FlatpakOpen => Self::FlatpakOpen { app },
            ActionKind::NeedsFlatpakHost => Self::FlatpakSetup { app },
            ActionKind::AddBot => Self::AddBot { app },
        })
    }

    pub fn as_package(&self) -> Option<(&str, ActionKind)> {
        let (app, action) = match self {
            Self::InstallRuntime { app, .. } => (app, ActionKind::RuntimeInstall),
            Self::GetApp { app } => (app, ActionKind::Get),
            Self::OpenApp { app } => (app, ActionKind::Open),
            Self::Update { app } => (app, ActionKind::Update),
            Self::UninstallApp { app } => (app, ActionKind::Remove),
            Self::Pin { app } => (app, ActionKind::MarkInstalled),
            Self::Unpin { app } => (app, ActionKind::Unmark),
            Self::FlatpakInstall { app } => (app, ActionKind::FlatpakInstall),
            Self::FlatpakUpdate { app } => (app, ActionKind::FlatpakUpdate),
            Self::FlatpakOpen { app } => (app, ActionKind::FlatpakOpen),
            Self::FlatpakSetup { app } => (app, ActionKind::NeedsFlatpakHost),
            Self::AddBot { app } => (app, ActionKind::AddBot),
            _ => return None,
        };
        Some((app.as_str(), action))
    }
}

const BOTS_LINK: &str = "tilo-store:bots";

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

#[derive(Clone, Debug)]
pub struct CaptureCatalogue {
    pub items: Vec<crate::StoreItem>,
}

const SCENARIOS: &[&str] = &[
    "wide",
    "narrow",
    "search",
    "detail",
    "detail-narrow",
    "detail-technical",
    "detail-update",
    "detail-flatpak",
    "detail-blocked",
    "updates",
    "installed",
    "bots",
    "category",
    "remotes",
    "account-setup",
    "account-remotes",
    "remotes-disabled",
    "search-notes",
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

const SHOT_REPORTED_APP: &str = "so.tilo.notes";

ui::embedded_catalog!(pub(crate) static CATALOG, ["en-US", "de-DE"]);

pub struct StoreShell;

impl ResourceApp for StoreShell {
    type Resource = Reports;
    const ID: &'static str = ui::apps::STORE;
    const TITLE: &'static str = "App Store";
    const TITLE_KEY: &'static str = "store-app-title";
    const ICON: Option<&'static str> = Some("store");
    const OPENS: &'static [&'static str] = &[
        "application/vnd.flatpak.ref",
        "application/vnd.tilo.package",
    ];
    const ARGUMENTS: usize = 1;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn view(cx: &mut ViewCx<Reports>) -> Element {
        let dispatcher = cx.dispatcher();
        cx.on_open(
            move |anchor: &str| -> Result<(), ui::tilo_resource::Refusal> {
                dispatcher.effect(&crate::search::opened(anchor)?);
                Ok(())
            },
        );
        crate::app::window(cx, StoreApp::new())
    }

    /// A Store link opens what it names: the catalogue, the bots, an app's
    /// page (`tilo-store:app/<id>`) or a runtime's.
    fn open_link(link: &str) -> Option<Vec<Invoke<Reports>>> {
        let effect = if link == "tilo-store:catalogue" {
            StoreEffect::ShowCatalogue
        } else if link == BOTS_LINK {
            StoreEffect::ShowView {
                view: "bots".to_owned(),
            }
        } else if let Some(runtime) = link.strip_prefix("tilo-store:runtime/") {
            StoreEffect::ShowRuntimeDetails {
                runtime: runtime.to_owned(),
                registry: None,
            }
        } else {
            match parse_store_uri(link) {
                Ok(app) => StoreEffect::ShowDetails { app },
                Err(error) => {
                    tracing::warn!(%error, "ignoring the App Store link");
                    return None;
                }
            }
        };
        Some(vec![Invoke::effect(&effect)])
    }

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
            let path = match AbsolutePath::new(argument) {
                Ok(path) => path,
                Err(error) => {
                    tracing::warn!(%error, "ignoring the opened file");
                    return Vec::new();
                }
            };
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("tapp"))
            {
                StoreEffect::ImportPackage {
                    path: Some(path),
                    content: None,
                    reported_size: None,
                }
            } else {
                StoreEffect::OpenReference {
                    path: Some(path),
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

    fn fixture(fixture: &mut Fixture<Reports>) {
        let items = if matches!(fixture.scenario(), Some("empty" | "offline")) {
            Vec::new()
        } else {
            crate::fixture::capture_catalog(fixture.scenario())
        };
        fixture.provide(CaptureCatalogue { items });
    }

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

pub fn api() -> PublishedResource {
    let mut api =
        tilo_resource::publish::<Reports>(Reports::KIND, Reports::LABEL, &Reports::default());
    api.actions.extend(StoreEffect::specs());
    api
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::testing::TestApp;
    use ui::tilo_actions::OperationKind;

    #[test]
    fn moderation_disclosures_require_framework_confirmation() {
        for action in ["submit-report", "appeal"] {
            let spec = StoreEffect::specs()
                .into_iter()
                .find(|spec| spec.id == action)
                .unwrap();
            assert_eq!(spec.contract.kind, ui::tilo_actions::ActionKind::Outside);
            assert_eq!(
                spec.contract.confirmation,
                ui::tilo_actions::ConfirmationPolicy::Confirm
            );
            assert_eq!(spec.contract.risk, ui::tilo_actions::RiskLevel::Sensitive);
        }
    }

    struct Offline {
        env: tilo_test_env::ScopedEnv,
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
        Offline { env, dir }
    }

    struct AvailableFeed {
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for AvailableFeed {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            self.worker.take().unwrap().join().unwrap();
        }
    }

    fn available_feed(environment: &mut Offline) -> AvailableFeed {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        environment.env.set(
            "FLATHUB_API_BASE",
            format!("http://{}", listener.local_addr().unwrap()),
        );
        listener.set_nonblocking(true).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = std::sync::Arc::clone(&stop);
        let worker = std::thread::spawn(move || {
            while !stopping.load(std::sync::atomic::Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(tilo_test_env::hang_bound()))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(tilo_test_env::hang_bound()))
                            .unwrap();
                        let mut request = [0; 4096];
                        let length = stream.read(&mut request).unwrap();
                        let request = String::from_utf8_lossy(&request[..length]);
                        assert!(
                            request.starts_with("GET /collection/popular?")
                                || request.starts_with("POST /search?"),
                            "{request}"
                        );
                        let body = r#"{"hits":[],"totalPages":1}"#;
                        write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("feed accept failed: {error}"),
                }
            }
        });
        AvailableFeed {
            stop,
            worker: Some(worker),
        }
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

    tilo_test_env::scenario! {
        fn a_store_link_names_an_app_and_only_opens_its_page;

        phase a_store_link_opens_the_app_page_and_installs_nothing;
        {
            let invokes = StoreShell::open_link("tilo-store:app/com.spotify.Client")
                .expect("a Store link opens");
            let expected = Invoke::<Reports>::effect(&StoreEffect::ShowDetails {
                app: "com.spotify.Client".into(),
            });
            assert_eq!(invokes.len(), 1, "one step: {invokes:?}");
            assert_eq!(invokes[0].id, expected.id);
            assert_eq!(invokes[0].arguments, expected.arguments);
        }

        phase store_links_name_only_a_stable_app_identity;
        {
            // An identity is at most 255 bytes of non-empty `.`-separated
            // segments (single segments and underscores for Flathub ids
            // allowed; empty segments and leading or trailing dots refused).
            let longest = "a".repeat(255);
            let longest_link = format!("tilo-store:app/{longest}");
            let too_long_link = format!("tilo-store:app/{longest}a");
            for (link, app) in [
                ("tilo-store:app/so.tilo.notes", Some("so.tilo.notes")),
                (
                    "tilo-store:app/org.gnome.Chess-2",
                    Some("org.gnome.Chess-2"),
                ),
                (
                    "tilo-store:app/org.example.My_App",
                    Some("org.example.My_App"),
                ),
                (
                    "tilo-store:app/-org.example.App",
                    Some("-org.example.App"),
                ),
                ("tilo-store:app/a", Some("a")),
                (longest_link.as_str(), Some(longest.as_str())),
                (too_long_link.as_str(), None),
                ("tilo-store:app/.so.tilo", None),
                ("tilo-store:app/so.tilo.", None),
                ("tilo-store:app/so..tilo", None),
                ("tilo-store:app/..", None),
                ("tilo-store:app/", None),
                ("tilo-store:app/so.tilo.notes?install=true", None),
                ("tilo-store:remote/so.tilo.notes", None),
                ("", None),
                ("tilo-store:app", None),
                ("tilo-store:catalogue", None),
                ("TILO-STORE:app/so.tilo.notes", None),
                (" tilo-store:app/so.tilo.notes", None),
                ("tilo-store:app/so.tilo.notes/extra", None),
                ("tilo-store:app/../so.tilo.notes", None),
                ("tilo-store:app/so.tilo.notes#install", None),
                ("tilo-store:app/so.tilo notes", None),
                ("tilo-store:app/so.tilo.n\u{f6}tes", None),
                ("tilo-store:app/so.tilo.notes\n", None),
            ] {
                let parsed = parse_store_uri(link);
                assert_eq!(parsed.as_deref().ok(), app, "{link:?} gave {parsed:?}");
            }
        }
    }

    /// One published tree: the verbs, field edits and effects on the
    /// resource, and only effects on the live rows, a report's among them.
    #[test]
    fn the_store_publishes_one_tree() {
        let mut app = TestApp::<StoreShell>::capture(Some("moderation-case")).unwrap();
        let resources = app.harness().resources();
        let specs: Vec<_> = resources
            .iter()
            .filter(|resource| resource.kind == Reports::KIND)
            .collect();
        assert_eq!(specs.len(), 1, "one Store resource");
        assert!(
            specs[0]
                .actions
                .iter()
                .any(|action| action.id == "draft-report")
        );
        assert!(
            specs[0]
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
    /// on the catalogue, its remotes and its trust page. The catalogue's
    /// actions are the window bar's slots: each is enabled there, and a
    /// press on one runs its published action as a control's would.
    #[test]
    fn every_control_binds_a_published_action() {
        let _offline = offline();
        let mut app = TestApp::<StoreShell>::new().unwrap();
        app.settle();
        for (page, effect) in [
            ("catalogue", None),
            ("remotes", None),
            ("trust", Some(StoreEffect::ShowTrust)),
        ] {
            if let Some(effect) = effect {
                app.effect(&effect).unwrap();
                app.settle();
            }
            if page == "catalogue" {
                let toolbar = ui::window_toolbar::declared().expect("the Store declares its bar");
                for name in [
                    "Review package",
                    "Flatpak repositories",
                    "Trust and moderation",
                ] {
                    let slot = toolbar
                        .actions
                        .iter()
                        .find(|action| action.name == name)
                        .unwrap_or_else(|| panic!("catalogue: missing bar slot {name}"));
                    assert!(slot.enabled, "catalogue: live owner slot {name}");
                    assert!(slot.menu, "catalogue: {name} is a More menu item");
                }
            }
            if page == "remotes" {
                app.harness()
                    .press_window_action("Flatpak repositories")
                    .unwrap();
                app.settle();
                let toolbar = ui::window_toolbar::declared().expect("the Store declares its bar");
                assert_eq!(
                    toolbar
                        .location
                        .as_ref()
                        .map(|location| location.label.as_str()),
                    Some("Flatpak repositories"),
                    "the bar's remotes slot opens the repositories"
                );
                // Back is the window bar's slot, not a row the page draws.
                assert!(
                    toolbar
                        .leading
                        .iter()
                        .any(|action| action.name == "Back" && action.enabled),
                    "remotes: the window bar's Back returns to the catalogue: {toolbar:?}"
                );
                assert!(
                    toolbar
                        .actions
                        .iter()
                        .any(|action| action.name == "Refresh repositories" && action.enabled),
                    "remotes: refreshing the repositories is the window bar's: {toolbar:?}"
                );
            }
            let audit = app.harness().native_app_audit();
            let bindings =
                ui::widget::controls::control_action_bindings(app.harness().root_widget());
            if page == "catalogue" {
                assert_bar_search_reaches_the_store(&mut app);
            } else {
                assert!(!bindings.is_empty(), "{page}");
            }
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
                let targets = app.harness().targets();
                assert!(
                    !targets.contains_key("store.search"),
                    "search is the window bar's, not a field of the Store's: {targets:?}"
                );
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
            &[],
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
            surface.state().set_flatpak_available(true);
            surface.state().set_items(vec![selected]);
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
            .contract
            .clone();
        assert_eq!(contract.result, ui::tilo_actions::ResultSpec::Job);
        let invocation = app
            .start_effect_on(&row, &StoreEffect::FlatpakOpen { app: id.clone() })
            .unwrap();
        let mut request = None;
        tilo_test_env::wait_until("the original channel carries the admitted launch", || {
            app.settle();
            // The host takes the window's requests every frame it runs, and
            // the test harness is that host here: what the Store asked of the
            // original channel is what it took, whether it was taken in the
            // frame the launch landed in or the one after.
            request = app
                .harness()
                .take_collaboration_requests()
                .into_iter()
                .find(|request| {
                    matches!(request, CollabRequest::LaunchInstalledApplication { .. })
                });
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
            surface.state().set_worker_busy(false);
            assert!(
                surface
                    .state()
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
            surface
                .state()
                .handle_collab_event(&CollabEvent::InstalledApplicationLaunchResult {
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
            .contract
            .clone();
        assert_eq!(contract.result, ui::tilo_actions::ResultSpec::Job);
        let versions = app.versions().len();
        let invocation = app.start_effect(&StoreEffect::Refresh).unwrap();
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| {
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
            app.with_surface::<StoreApp, _>(|surface| surface.state().status_line().to_owned()),
            Some("Store refreshed.".to_owned())
        );
        assert_eq!(app.versions().len(), versions, "refresh writes no document");
    }

    fn settle_catalogue_worker(app: &mut TestApp<StoreShell>) {
        tilo_test_env::wait_until("the ordinary catalogue worker settles", || {
            app.settle();
            app.with_surface::<StoreApp, _>(|surface| {
                surface.state().inspect_catalogue(None).unwrap()["worker_busy"] == false
            })
            .unwrap()
        });
    }

    /// A device's package cache whose repository never answers a catalogue until the cache
    /// itself goes away.
    #[derive(Default)]
    struct SlowCache {
        /// The catalogue reading the Store asked for, unanswered.
        asked: std::sync::Mutex<
            Vec<
                ui::tilo_actions::remote::Completer<
                    Result<
                        ui::tilo_actions::remote::InvocationReceipt,
                        ui::tilo_actions::remote::ActionCallError,
                    >,
                >,
            >,
        >,
    }

    impl SlowCache {
        fn asked(&self) -> bool {
            !self.asked.lock().unwrap().is_empty()
        }

        /// The cache goes away with the reading unanswered.
        fn vanish(&self) {
            self.asked.lock().unwrap().clear();
        }
    }

    impl ui::tilo_actions::remote::ActionProvider for SlowCache {
        fn serves(&self, provider: &str) -> bool {
            provider == "so.tilo.packages"
        }

        fn query(
            &self,
            _caller: &ui::tilo_actions::remote::Caller,
            query: ui::tilo_actions::remote::CatalogueQuery,
        ) -> ui::tilo_actions::remote::Pending<
            Result<
                ui::tilo_actions::remote::CataloguePage,
                ui::tilo_actions::remote::ActionCallError,
            >,
        > {
            use ui::tilo_actions::remote::{
                CataloguePage, CatalogueResource, CatalogueRow, Pending,
            };
            if query.provider.as_deref() != Some("so.tilo.packages") {
                return Pending::ready(Ok(CataloguePage::default()));
            }
            Pending::ready(Ok(CataloguePage {
                revision: "packages".into(),
                rows: vec![CatalogueRow {
                    provider: "so.tilo.packages".into(),
                    app_id: "so.tilo.packages".into(),
                    resource: CatalogueResource {
                        id: "devices/11111111111111111111111111111111/packages".into(),
                        kind: "devices.packages".into(),
                        label: "Packages".into(),
                        presentation: None,
                    },
                    ..CatalogueRow::default()
                }],
                exhausted: true,
                ..CataloguePage::default()
            }))
        }

        fn invoke(
            &self,
            _caller: &ui::tilo_actions::remote::Caller,
            request: ui::tilo_actions::remote::InvokeRequest,
        ) -> ui::tilo_actions::remote::Pending<
            Result<
                ui::tilo_actions::remote::InvocationReceipt,
                ui::tilo_actions::remote::ActionCallError,
            >,
        > {
            assert_eq!(request.target.address.action, "catalogue");
            let (asked, answer) = ui::tilo_actions::remote::pending();
            self.asked.lock().unwrap().push(asked);
            answer
        }

        fn observe(
            &self,
            _caller: &ui::tilo_actions::remote::Caller,
            _id: &ui::tilo_actions::InvocationId,
        ) -> Option<ui::tilo_actions::remote::InvocationState> {
            None
        }

        fn cancel(
            &self,
            _caller: &ui::tilo_actions::remote::Caller,
            _id: &ui::tilo_actions::InvocationId,
        ) -> ui::tilo_actions::remote::Pending<Result<(), ui::tilo_actions::remote::ActionCallError>>
        {
            ui::tilo_actions::remote::Pending::ready(Ok(()))
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

    fn assert_bar_search_reaches_the_store(app: &mut TestApp<StoreShell>) {
        app.settle();
        assert_eq!(
            app.with_surface::<StoreApp, _>(|surface| ui::widget::InPlace::of(surface)),
            Some(ui::widget::InPlace::Query),
            "the pending-source view keeps the window bar's search on the Store"
        );
    }

    /// The one repository a Store test's slow package cache is asked for a catalogue of.
    fn slow_registries() -> Vec<crate::config::RegistryConfig> {
        vec![crate::config::RegistryConfig {
            name: "slow-0".into(),
            base_url: "https://registry.example".into(),
            owner_pubkey_hex: "11".repeat(32),
            enabled: true,
            channel: app_cli::ReleaseChannel::Stable,
        }]
    }

    /// A package cache that goes away while a repository's catalogue is unanswered settles the
    /// refresh, and the window says which repository gave nothing, with no made-up success.
    #[test]
    fn disconnected_source_settles_refresh_and_reports_the_transport_error() {
        let _offline = offline();
        let mut app = TestApp::<StoreShell>::new().unwrap();
        settle_catalogue_worker(&mut app);
        let cache = std::sync::Arc::new(SlowCache::default());
        let _entered = ui::RemoteActions::enter(
            std::sync::Arc::clone(&cache)
                as std::sync::Arc<dyn ui::tilo_actions::remote::ActionProvider + Send + Sync>,
            ui::tilo_actions::remote::Caller::local_person("ada"),
        );
        app.with_surface::<StoreApp, _>(|surface| {
            surface
                .state()
                .connect_test_actions(ui::RemoteActions::current(), slow_registries())
        })
        .unwrap()
        .unwrap();
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |_| cache.asked())
        );
        cache.vanish();
        settle_catalogue_worker(&mut app);
        assert!(
            app.with_surface::<StoreApp, _>(|surface| {
                surface.state().inspect_catalogue(None).unwrap()["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("slow"))
            })
            .unwrap(),
            "the actual source disconnect remains visible, with no fake success result"
        );
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
                .wait_until(tilo_test_env::hang_bound(), |harness| {
                    harness
                        .resources()
                        .iter()
                        .any(|resource| resource.id.as_str() == Reports::KIND)
                })
        );
        settle_catalogue_worker(&mut app);
        app.with_surface::<StoreApp, _>(|surface| {
            let snapshot = surface.state().inspect_catalogue(None).unwrap();
            assert!(snapshot["imported_package"].is_null());
            assert_eq!(snapshot["error"], CATALOG.t("store-package-offer-required"));
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
                .wait_until(tilo_test_env::hang_bound(), |harness| {
                    harness
                        .resources()
                        .iter()
                        .any(|resource| resource.id.as_str() == Reports::KIND)
                })
        );
        settle_catalogue_worker(&mut app);
        app.with_surface::<StoreApp, _>(|surface| {
            let snapshot = surface.state().inspect_catalogue(None).unwrap();
            assert!(snapshot["imported_package"].is_null());
            assert_eq!(snapshot["error"], CATALOG.t("store-package-size-refused"));
        })
        .unwrap();
    }

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
            .wait_until(tilo_test_env::hang_bound(), |harness| {
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

    #[test]
    fn the_bots_link_opens_the_bots_and_nothing_else() {
        let _offline = offline();
        let mut app = TestApp::<StoreShell>::launched(&[BOTS_LINK]).unwrap();
        let observed = app
            .effect(&StoreEffect::InspectCatalogue { app: None })
            .unwrap()
            .value
            .unwrap();
        assert_eq!(
            observed.get("view").and_then(serde_json::Value::as_str),
            Some("bots"),
            "{observed}"
        );
        assert!(
            app.effect_progress().is_empty(),
            "opening the bots admits no package job"
        );
    }

    #[test]
    fn launching_on_a_link_shows_that_app() {
        let _offline = offline();
        let mut app = TestApp::<StoreShell>::launched(&["tilo-store:app/so.tilo.unknown"]).unwrap();
        let asked = app
            .harness()
            .wait_until(tilo_test_env::hang_bound(), |harness| {
                harness
                    .inspection_json()
                    .is_ok_and(|frame| frame.contains("so.tilo.unknown"))
            });
        assert!(asked, "the window asks for the linked app");
    }

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

    /// The catalogue is found in the Store's own search while it runs and
    /// by desktop search from the shared catalogue; none of it is kept in
    /// the system's search index, which would list every app twice.
    #[test]
    fn the_catalogue_is_searchable_but_never_kept_in_the_search_index() {
        let mut wide = TestApp::<StoreShell>::capture(Some("wide")).unwrap();
        let apps: Vec<String> = wide
            .surface_mut::<StoreApp>()
            .state()
            .catalogue_search_entries()
            .into_iter()
            .filter_map(|entry| entry.anchor)
            .collect();
        assert!(!apps.is_empty(), "the running Store finds its catalogue");
        let kept = wide.search_index_entries();
        assert!(
            kept.iter().all(|entry| !apps.contains(&entry.resource)),
            "no catalogue app is kept in the search index: {kept:?}"
        );
    }

    #[test]
    fn publisher_metadata_reaches_views_and_semantic_inspection() {
        let _offline = offline();
        let mut app = TestApp::<StoreShell>::new().unwrap();
        tilo_test_env::wait_until("the metadata journey's catalogue worker settles", || {
            app.settle();
            app.with_surface::<StoreApp, _>(|surface| {
                surface.state().inspect_catalogue(None).unwrap()["worker_busy"] == false
            })
            .unwrap()
        });
        app.with_surface(|store: &mut StoreApp| {
            store
                .state()
                .show_capture_catalogue(crate::fixture::fixture_catalog());
            store.state().stage_scene(Some("detail"));
        })
        .unwrap();
        let shown = frame(&mut app);
        assert!(shown.contains("Notes you write alone or together"));
        assert!(shown.contains("Capture ideas, organise projects"));
        assert!(shown.contains("Supported languages"));
        assert!(shown.contains("en, es"));
        let resources = app.harness().resources();
        let notes = find(&resources, "app:so.tilo.notes").unwrap();
        assert_eq!(
            notes.presentation.unwrap().detail.as_deref(),
            Some("Notes you write alone or together")
        );
        let terminal = find(&resources, "app:so.tilo.terminal").unwrap();
        assert_eq!(
            terminal.presentation.unwrap().detail.as_deref(),
            Some("A terminal for your shell and command-line tools")
        );
        let inspection = app
            .effect(&StoreEffect::InspectCatalogue {
                app: Some("so.tilo.notes".into()),
            })
            .unwrap();
        let value = inspection.value.unwrap();
        assert_eq!(
            value["detail"]["tagline"],
            "Notes you write alone or together"
        );
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
}

ui::reach_tests!(
    StoreShell,
    shares: [share_0, share_1, share_2, share_3, share_4, share_5, share_6, share_7],
);

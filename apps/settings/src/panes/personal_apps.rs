//! Personal default applications: which installed app opens each kind of
//! file and link for this person, wherever they sign in. The choices are the
//! `default_apps` section of the person's settings, which the launcher reads
//! when it opens a file or a link. Workspace policy deliberately does not live
//! here: it is contextual and belongs to the shell's Workspace Settings.

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

use desktop_policy::{AppCandidate, DefaultAppResolver, PersonalDefaults, ResourceKind};
use ui::LocalState;
use ui::framework::{Actions, EffectAnswer, EffectCx, Effected, LiveRow, ViewCx};
use ui::i18n::ArgValue;
use ui::tilo_actions::ResourcePresentation;
use ui::tilo_resource::{self, Effect};
use ui::widget::{Button, Dropdown, Element};

use crate::area::AreaContext;
use crate::effects::{SettingsEffect, effect};
use crate::i18n::{t, t_args};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{
    note, rows, section, setting_row, setting_row_described, untitled,
};
use ui::storage::SettingsCache;

crate::effects::effect_family!(DefaultAppsEffect => answer);

crate::area::pane_area!(DefaultAppsArea);

/// The window's [`DefaultAppsArea`].
pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&DefaultAppsArea> {
    app.areas().find()
}

/// The most kinds of file and link the published tree lists.
const MAX_KIND_ROWS: usize = 256;
/// The most apps one kind lists.
const MAX_APP_ROWS: usize = 128;

pub use crate::prefs::DefaultApps;

/// The defaults `apps` holds, by the kinds they are kept for.
fn personal(apps: &DefaultApps) -> PersonalDefaults {
    PersonalDefaults {
        handlers: apps
            .handlers
            .iter()
            .filter_map(|(kind, app)| Some((stored_kind(kind)?, app.clone())))
            .collect(),
    }
}

/// Which app opens each kind of file and link.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum DefaultAppsEffect {
    /// Open a kind of file or link with an installed app.
    #[effect(idempotent)]
    #[effect(keywords = "default app, open with, file association")]
    SetDefaultApp {
        #[param("The kind of file or link: `ext:pdf`, `mime:text/html`, `scheme:https`")]
        kind: String,
        #[param("The installed app's id")]
        app: String,
    },
    /// Open the workspace's own app defaults in Workspace Settings.
    #[effect(job)]
    OpenWorkspaceDefaults,
}

/// Answer `effect`: only an installed app that claims the kind is accepted;
/// the shell is asked to open Workspace Settings off the UI thread.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: DefaultAppsEffect,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let (kind, package) = match effect {
        DefaultAppsEffect::SetDefaultApp { kind, app } => (kind, app),
        DefaultAppsEffect::OpenWorkspaceDefaults => {
            if !app.reaches_devices() {
                return Err("only a window someone uses opens workspace settings".into());
            }
            return Ok(app.job(
                cx,
                open_workspace_defaults,
                |_: &SettingsApp, opened: Result<(), String>, _| opened.map(|()| Effected::done()),
            ));
        }
    };
    let resource = ResourceKind::parse(&kind)
        .ok_or_else(|| format!("`{kind}` is not a kind of file or link"))?;
    let area = crate::panes::personal_apps::area(app).ok_or_else(crate::area::missing)?;
    let resolver = DefaultAppResolver::new(area.candidates());
    PersonalDefaults::default()
        .set(resource, package.clone(), &resolver)
        .map_err(|error| error.to_string())?;
    // The section's verb changes only the chosen kind in the latest
    // settings, not a cached resolver snapshot that may omit newer or
    // unrecognized handlers.
    app.sections()
        .invoke::<DefaultApps>(DefaultApps::OPEN_WITH.with((kind, package)))
        .map(|()| Effected::done())
}

/// The kind a stored handler names.
fn stored_kind(kind: &str) -> Option<ResourceKind> {
    serde_json::from_value(serde_json::Value::String(kind.to_owned())).ok()
}

/// What the Default Apps area keeps for the window: the choices as last
/// read, and the installed apps that claim kinds of files and links.
pub struct DefaultAppsArea {
    prefs: RefCell<SettingsCache<DefaultApps>>,
    candidates: OnceCell<Vec<AppCandidate>>,
    sample: bool,
    /// The installed apps, read off the UI thread after the window's first
    /// frame ([`Self::start_read`]).
    pub(crate) read: crate::device_read::DeviceRead<Vec<AppCandidate>>,
    /// The rows, kept for the defaults version and locale they show.
    rows: crate::effects::RowsMemo<(ui::storage::SettingsRevision, u64)>,
}

impl DefaultAppsArea {
    /// The area over `cx`'s settings. A capture lists sample apps rather
    /// than the ones this device has installed.
    pub fn new(cx: &AreaContext) -> Self {
        Self {
            prefs: RefCell::new(SettingsCache::new(cx.storage.settings::<DefaultApps>())),
            candidates: OnceCell::new(),
            sample: !cx.devices,
            read: crate::device_read::DeviceRead::default(),
            rows: crate::effects::RowsMemo::default(),
        }
    }

    /// The area among `candidates` in place of the apps installed.
    #[cfg(test)]
    fn over(cx: &AreaContext, candidates: Vec<AppCandidate>) -> Self {
        Self {
            candidates: OnceCell::from(candidates),
            ..Self::new(cx)
        }
    }

    fn candidates(&self) -> &[AppCandidate] {
        self.candidates.get_or_init(|| {
            self.read
                .take()
                .unwrap_or_else(|| Self::read_candidates(self.sample))
        })
    }

    /// Every installed app's manifest claims, or the sample a capture lists.
    fn read_candidates(sample: bool) -> Vec<AppCandidate> {
        if sample {
            sample_candidates()
        } else {
            installed_candidates()
        }
    }

    /// Read the installed apps off the UI thread; `false` when they have
    /// been read or are being read.
    pub(crate) fn start_read(&self) -> bool {
        if self.is_open() {
            return false;
        }
        let sample = self.sample;
        self.read.start(move || Self::read_candidates(sample))
    }

    /// Whether the area has read what its rows show: its pane was shown,
    /// an effect or a row lookup reached it, or the window read it after
    /// its first frame ([`crate::panes::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.candidates.get().is_some()
    }

    /// One live row per kind of file or link, showing the app that opens it
    /// now, with a row per installed app that claims it offering to make it
    /// the default.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let mut prefs = self.prefs.borrow_mut();
        prefs.refresh();
        let key = (prefs.revision(), ui::i18n::I18nManager::revision());
        self.rows.rows(key, || self.make_rows(prefs.get()))
    }

    fn make_rows(&self, prefs: &DefaultApps) -> Vec<LiveRow> {
        let defaults = personal(prefs);
        let candidates = self.candidates();
        let resolver = DefaultAppResolver::new(candidates);
        kinds(candidates)
            .into_iter()
            .take(MAX_KIND_ROWS)
            .map(|kind| {
                let id = format!("settings.default-app:{}", kind.as_str());
                let current = resolver.resolve(&kind, "", &defaults, None);
                let mut row = LiveRow::new(
                    id.clone(),
                    "settings.default-app-handler",
                    kind.display_label(),
                );
                if let Some(current) = &current {
                    row = row.presentation(ResourcePresentation {
                        detail: Some(current.package_id.clone()),
                        ..ResourcePresentation::default()
                    });
                }
                for app in apps_for(candidates, &kind).into_iter().take(MAX_APP_ROWS) {
                    let mut choose = SettingsEffect::from(DefaultAppsEffect::SetDefaultApp {
                        kind: kind.as_str().to_owned(),
                        app: app.package_id.clone(),
                    })
                    .bound();
                    choose.type_mut().label =
                        format!("Open {} with {}", resource_label(&kind), app.display_name);
                    row = row.row(
                        LiveRow::new(
                            format!("{id}/{}", app.package_id),
                            "settings.default-app-candidate",
                            app.display_name.clone(),
                        )
                        .operation(choose),
                    );
                }
                row
            })
            .collect()
    }
}

/// The apps a capture lists.
fn sample_candidates() -> Vec<AppCandidate> {
    let claim = |package: &str, name: &str, kinds: Vec<ResourceKind>| AppCandidate {
        package_id: package.into(),
        display_name: name.into(),
        resources: kinds.into_iter().collect(),
        secondary: false,
    };
    let ext = |ext: &str| ResourceKind::extension(ext).ok();
    let scheme = |scheme: &str| ResourceKind::scheme(scheme).ok();
    vec![
        claim(
            "so.tilo.browser",
            "Browser",
            [scheme("https"), ext("html")]
                .into_iter()
                .flatten()
                .collect(),
        ),
        claim(
            "so.tilo.pdf",
            "PDF Viewer",
            ext("pdf").into_iter().collect(),
        ),
        claim(
            "so.tilo.photos",
            "Photos",
            [ext("jpg"), ext("png")].into_iter().flatten().collect(),
        ),
    ]
}

/// Every kind some candidate claims, in order.
fn kinds(candidates: &[AppCandidate]) -> Vec<ResourceKind> {
    let mut kinds: Vec<_> = candidates
        .iter()
        .flat_map(|app| app.resources.iter().cloned())
        .collect();
    kinds.sort();
    kinds.dedup();
    kinds
}

/// The candidates that claim `kind`, by name.
fn apps_for<'a>(candidates: &'a [AppCandidate], kind: &ResourceKind) -> Vec<&'a AppCandidate> {
    let mut apps: Vec<_> = candidates.iter().filter(|app| app.handles(kind)).collect();
    apps.sort_by(|a, b| {
        a.display_name
            .cmp(&b.display_name)
            .then_with(|| a.package_id.cmp(&b.package_id))
    });
    apps
}

/// The app that opens `kind` now among `candidates`: the person's choice,
/// else the one the resolver picks, which is shown, not stored.
fn shown_app(
    candidates: &[AppCandidate],
    defaults: &PersonalDefaults,
    kind: &ResourceKind,
) -> String {
    DefaultAppResolver::new(candidates)
        .resolve(kind, "", defaults, None)
        .and_then(|selected| {
            candidates
                .iter()
                .find(|app| app.package_id == selected.package_id)
        })
        .map(option)
        .unwrap_or_default()
}

/// What a kind's rows call it: "PDF documents", "Secure web links".
fn resource_label(resource: &ResourceKind) -> String {
    if let Some(ext) = resource.extension_name() {
        t_args(
            "settings-personal-apps-ext-files",
            &[("ext", ArgValue::Text(ext.to_string()))],
        )
    } else if let Some(scheme) = resource.scheme_name() {
        t_args(
            "settings-personal-apps-scheme-links",
            &[("scheme", ArgValue::Text(scheme.to_string()))],
        )
    } else {
        resource.display_label()
    }
}

/// What a chooser stores for `app`: its package, shown by its name.
fn option(app: &AppCandidate) -> String {
    app.package_id.clone()
}

/// The Default apps page as view code over the person's choices
/// ([`DefaultApps`]), among the apps the area read as installed.
pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let candidates: Rc<[AppCandidate]> = area(app)?.candidates().into();
    let actions = cx.actions();
    let section = cx.settings::<DefaultApps>();
    cx.record(&section, move |cx| {
        let (candidates, actions) = (Rc::clone(&candidates), actions.clone());
        // A changed choice rebuilds the choosers, as each names the app it
        // shows.
        cx.switch(&cx.value(DefaultApps::clone), move |_, apps| {
            page(&actions, &candidates, apps)
        })
    })
}

/// The page over the person's choices as they are now (`apps`): a chooser
/// per kind some installed app claims, and the way to the workspace's own
/// rules.
fn page(actions: &Actions<Settings>, candidates: &[AppCandidate], apps: &DefaultApps) -> Element {
    let defaults = personal(apps);
    let mut file_rows = Vec::new();
    for kind in kinds(candidates) {
        let apps = apps_for(candidates, &kind);
        if apps.is_empty() {
            continue;
        }
        let state = LocalState::new(shown_app(candidates, &defaults, &kind));
        let options: Vec<String> = apps.iter().map(|a| option(a)).collect();
        let named_options: Vec<(String, String)> = apps
            .iter()
            .map(|app| (option(app), app.display_name.clone()))
            .collect();
        let shown_name = named_options
            .iter()
            .find(|(package, _)| *package == state.get())
            .map_or_else(|| state.get(), |(_, name)| name.clone());
        let named = kind.as_str().to_owned();
        let mut chooser = Dropdown::from_state(options, &state)
            .with_id(&format!("personal-default.{}", kind.as_str()))
            .with_placeholder(&shown_name);
        chooser.set_value_options(named_options);
        let picker = actions.bind_effect(
            chooser,
            effect(DefaultAppsEffect::SET_DEFAULT_APP)
                .map(move |package: String| (named.clone(), package)),
        );
        file_rows.push(setting_row(&resource_label(&kind), Box::new(picker)));
    }
    if file_rows.is_empty() {
        file_rows.push(note(&t("settings-personal-apps-empty")));
    } else {
        file_rows.push(note(&t("settings-personal-apps-choice-follows")));
    }
    // A workspace's own rules are that workspace's: one row opens them.
    let workspace = untitled(rows(vec![setting_row_described(
        &t("settings-personal-apps-workspace-rules"),
        &t("settings-personal-apps-scope-caption"),
        Box::new(actions.bind_effect_invocation(
            Button::new(
                "open-workspace-app-defaults",
                &t("settings-personal-apps-open"),
            ),
            &SettingsEffect::from(DefaultAppsEffect::OpenWorkspaceDefaults),
        )),
    )]));
    provider_settings_ui::widgets::page(vec![
        section(&t("settings-personal-apps-files-title"), rows(file_rows)),
        workspace,
    ])
}

/// Ask the shell to open Workspace Settings at its app defaults: a round
/// trip to the shell, so it runs off the UI thread.
fn open_workspace_defaults() -> Result<(), String> {
    crate::device_read::off_ui_thread();
    os_actions::open_tool(
        os_actions::ToolIntent::WorkspaceSettings {
            route: os_actions::WorkspaceSettingsRoute::AppsFiles,
        },
        None,
    )
}

/// Read real installed manifests. A type only appears if an installed GUI app
/// claims it; Settings does not invent handlers.
pub fn installed_candidates() -> Vec<AppCandidate> {
    let mut out = Vec::new();
    for entry in package_store::list_installed().unwrap_or_default() {
        let Ok(manifest) = tilo_app_package::view::read_manifest(&entry.manifest_path) else {
            continue;
        };
        if manifest.entrypoint.gui_bin().is_none() {
            continue;
        }
        let resources = manifest
            .opens_ext
            .iter()
            .filter_map(|e| ResourceKind::extension(e).ok())
            .chain(
                manifest
                    .opens_mime
                    .iter()
                    .filter_map(|mime| ResourceKind::mime(mime).ok()),
            )
            .chain(
                manifest
                    .opens_schemes
                    .iter()
                    .filter_map(|scheme| ResourceKind::scheme(scheme).ok()),
            )
            .collect();
        let package_id = if manifest.package_id.is_empty() {
            entry.id
        } else {
            manifest.package_id
        };
        out.push(AppCandidate {
            display_name: manifest
                .name
                .unwrap_or_else(|| package_store::display_name_for_package_id(&package_id)),
            package_id,
            resources,
            secondary: manifest.opens_secondary,
        });
    }
    out.sort_by(|a, b| {
        a.display_name
            .cmp(&b.display_name)
            .then_with(|| a.package_id.cmp(&b.package_id))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    fn candidate(id: &str, ext: &str) -> AppCandidate {
        AppCandidate {
            package_id: id.into(),
            display_name: id.into(),
            resources: [ResourceKind::extension(ext).unwrap()].into(),
            secondary: false,
        }
    }

    #[test]
    fn personal_apps_copy_comes_from_the_catalogue() {
        assert_eq!(
            t("settings-personal-apps-files-title"),
            "Open files and links with"
        );
        assert_eq!(
            resource_label(&ResourceKind::extension("pdf").unwrap()),
            "PDF documents"
        );
        assert_eq!(
            resource_label(&ResourceKind::scheme("https").unwrap()),
            "Secure web links"
        );
    }

    /// A choice is the person's `default_apps` section, which the launcher
    /// reads; an app that does not claim the kind is refused, and showing
    /// the resolver's pick stores nothing.
    #[test]
    fn a_default_app_is_the_persons_choice() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let settings = app.settings::<DefaultApps>();
        assert!(
            settings.get().handlers.is_empty(),
            "showing the resolver's pick is not a personal choice"
        );
        app.effect_on(
            "settings.default-app:ext:pdf/so.tilo.pdf",
            &SettingsEffect::from(DefaultAppsEffect::SetDefaultApp {
                kind: "ext:pdf".into(),
                app: "so.tilo.pdf".into(),
            }),
        )
        .expect("the app's row offers making it the default");
        let section = settings
            .document()
            .section(desktop_policy::PERSONAL_DEFAULTS_SECTION);
        assert_eq!(
            PersonalDefaults::from_section(&section)
                .handlers
                .get(&ResourceKind::extension("pdf").unwrap())
                .map(String::as_str),
            Some("so.tilo.pdf")
        );
        let refused = DefaultAppsEffect::SetDefaultApp {
            kind: "ext:pdf".into(),
            app: "so.tilo.photos".into(),
        };
        assert!(app.effect(&SettingsEffect::from(refused)).is_err());
        let surface = app.agent_surface().to_string();
        assert!(
            surface.contains("settings.default-app:scheme:https"),
            "{surface}"
        );
    }

    #[test]
    fn choosing_a_default_preserves_other_saved_handlers() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let settings = app.settings::<DefaultApps>();
        // Open the area's cached snapshot before another writer changes it.
        let _ = app.agent_surface();
        settings
            .update(|stored| {
                stored
                    .handlers
                    .insert("ext:txt".into(), "so.tilo.notes".into());
                // A newer reader may understand kinds this version cannot parse.
                stored
                    .handlers
                    .insert("future:document".into(), "so.tilo.future".into());
            })
            .unwrap();
        app.effect(&SettingsEffect::from(DefaultAppsEffect::SetDefaultApp {
            kind: "ext:pdf".into(),
            app: "so.tilo.pdf".into(),
        }))
        .expect("choose the PDF handler");
        let handlers = settings.get().handlers;
        assert_eq!(
            handlers.get("ext:pdf").map(String::as_str),
            Some("so.tilo.pdf")
        );
        assert_eq!(
            handlers.get("ext:txt").map(String::as_str),
            Some("so.tilo.notes")
        );
        assert_eq!(
            handlers.get("future:document").map(String::as_str),
            Some("so.tilo.future")
        );
    }

    /// The page shows the resolver's pick until the person chooses, then
    /// follows the choice whoever made it.
    #[test]
    fn the_page_follows_the_choices() {
        crate::area::replace_in_next_window(|context| {
            DefaultAppsArea::over(
                context,
                vec![candidate("so.tilo.a", "pdf"), candidate("so.tilo.b", "pdf")],
            )
        });
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::DefaultApps);
        app.settle();
        let shown = |app: &mut TestApp<SettingsShell>| {
            ui::widget::find_test_id(app.harness().root_widget(), "personal-default.ext:pdf")
                .and_then(ui::widget::Widget::accessibility)
                .and_then(|node| node.value)
        };
        assert_eq!(shown(&mut app).as_deref(), Some("so.tilo.a"));
        app.settings::<DefaultApps>()
            .update(|apps| {
                apps.handlers
                    .insert("ext:pdf".to_owned(), "so.tilo.b".into());
            })
            .unwrap();
        app.settle();
        assert_eq!(shown(&mut app).as_deref(), Some("so.tilo.b"));
    }

    /// A kind that is not one, or no app, is refused at the section.
    #[test]
    fn the_section_refuses_what_is_not_a_kind_or_an_app() {
        let mut apps = DefaultApps::default();
        assert!(apps.open_with("pdf".into(), "so.tilo.pdf".into()).is_err());
        assert!(apps.open_with("ext:pdf".into(), " ".into()).is_err());
        assert!(apps.handlers.is_empty());
        apps.open_with("ext:pdf".into(), "so.tilo.pdf".into())
            .unwrap();
        assert_eq!(
            apps.handlers.get("ext:pdf").map(String::as_str),
            Some("so.tilo.pdf")
        );
    }
}

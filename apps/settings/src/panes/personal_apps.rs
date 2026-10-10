//! Personal default applications: which installed app opens each kind of
//! file and link for this person, wherever they sign in. The choices are the
//! `default_apps` section of the person's settings, which the launcher reads
//! when it opens a file or a link. Workspace policy deliberately does not live
//! here: it is contextual and belongs to the shell's Workspace Settings.

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, HashMap};

use desktop_policy::{AppCandidate, DefaultAppResolver, PersonalDefaults, ResourceKind};
use serde::{Deserialize, Serialize};
use taffy::{NodeId, TaffyTree};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, LiveRow};
use ui::i18n::ArgValue;
use ui::theme::Theme;
use ui::tilo_actions::ResourcePresentation;
use ui::tilo_resource::{self, Effect};
use ui::widget::{AppModel, Button, Dropdown, ModelPath, Widget};

use crate::effects::{SettingsEffect, effect};
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{caption, card, choice_row_described, rows, value_row};

/// The most kinds of file and link the published tree lists.
const MAX_KIND_ROWS: usize = 256;
/// The most apps one kind lists.
const MAX_APP_ROWS: usize = 128;

/// The person's default apps, as their settings hold them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultApps {
    #[serde(default)]
    pub handlers: BTreeMap<ResourceKind, String>,
}

impl ui::storage::SettingsSchema for DefaultApps {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(desktop_policy::PERSONAL_DEFAULTS_SECTION);
}

impl DefaultApps {
    fn personal(&self) -> PersonalDefaults {
        PersonalDefaults {
            handlers: self.handlers.clone(),
        }
    }
}

/// Which app opens each kind of file and link.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum DefaultAppsEffect {
    /// Open a kind of file or link with an installed app.
    #[effect(idempotent)]
    SetDefaultApp {
        #[param("The kind of file or link: `ext:pdf`, `mime:text/html`, `scheme:https`")]
        kind: String,
        #[param("The installed app's id")]
        app: String,
    },
}

/// Answer `effect`: only an installed app that claims the kind is accepted.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: DefaultAppsEffect,
    _cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let DefaultAppsEffect::SetDefaultApp { kind, app: package } = effect;
    let resource =
        parse_kind(&kind).ok_or_else(|| format!("`{kind}` is not a kind of file or link"))?;
    let area = app.default_apps();
    let resolver = DefaultAppResolver::new(area.candidates());
    let mut defaults = area.prefs.borrow().get().personal();
    defaults
        .set(resource, package, &resolver)
        .map_err(|error| error.to_string())?;
    app.host().update(|stored: &mut DefaultApps| {
        stored.handlers = defaults.handlers;
    })
}

/// The kind a published id names: `ext:pdf`, `mime:text/html`,
/// `scheme:https`.
fn parse_kind(kind: &str) -> Option<ResourceKind> {
    let (prefix, name) = kind.split_once(':')?;
    match prefix {
        "ext" => ResourceKind::extension(name).ok(),
        "mime" => ResourceKind::mime(name).ok(),
        "scheme" => ResourceKind::scheme(name).ok(),
        _ => None,
    }
    .filter(|parsed| parsed.as_str() == kind)
}

/// What the Default Apps area keeps for the window: the choices as last
/// read, and the installed apps that claim kinds of files and links.
pub struct DefaultAppsArea {
    prefs: RefCell<Followed<DefaultApps>>,
    candidates: OnceCell<Vec<AppCandidate>>,
    sample: bool,
}

impl DefaultAppsArea {
    /// The area over `cx`'s settings. A capture lists sample apps rather
    /// than the ones this device has installed.
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            prefs: RefCell::new(cx.follow::<DefaultApps>()),
            candidates: OnceCell::new(),
            sample: !cx.reaches_devices(),
        }
    }

    fn candidates(&self) -> &[AppCandidate] {
        self.candidates.get_or_init(|| {
            if self.sample {
                sample_candidates()
            } else {
                installed_candidates()
            }
        })
    }

    /// Whether the area has read what its rows show: its pane was shown,
    /// an effect or a row lookup reached it, or the window read it after
    /// its first frame ([`crate::effects::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.candidates.get().is_some()
    }

    /// One live row per kind of file or link, showing the app that opens it
    /// now, with a row per installed app that claims it offering to make it
    /// the default.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let mut prefs = self.prefs.borrow_mut();
        prefs.refresh();
        let defaults = prefs.get().personal();
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
                    choose.label = format!(
                        "Open {} with {}",
                        PersonalAppsPane::resource_label(&kind),
                        app.display_name
                    );
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

pub struct PersonalAppsPane {
    cx: PaneCx,
    candidates: Vec<AppCandidate>,
    prefs: Followed<DefaultApps>,
    /// The app each kind's chooser shows, by kind.
    shown: HashMap<ResourceKind, LocalState<String>>,
    composed: ui::widget::Composed,
}

impl PersonalAppsPane {
    /// The pane over `cx`'s choices, among `area`'s apps.
    pub fn new(cx: PaneCx, area: &DefaultAppsArea) -> Self {
        let candidates = area.candidates().to_vec();
        let prefs = cx.follow::<DefaultApps>();
        let shown = kinds(&candidates)
            .into_iter()
            .map(|kind| (kind, LocalState::new(String::new())))
            .collect();
        let pane = Self {
            cx,
            candidates,
            prefs,
            shown,
            composed: ui::widget::Composed::default(),
        };
        pane.show();
        pane
    }

    /// Show the app that opens each kind now: the person's choice, else the
    /// one the resolver picks — which is shown, not stored.
    fn show(&self) {
        let defaults = self.prefs.get().personal();
        let resolver = DefaultAppResolver::new(&self.candidates);
        for (kind, state) in &self.shown {
            let current = resolver
                .resolve(kind, "", &defaults, None)
                .and_then(|selected| {
                    self.candidates
                        .iter()
                        .find(|app| app.package_id == selected.package_id)
                })
                .map(Self::option)
                .unwrap_or_default();
            state.set(current);
        }
    }

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

    fn option(app: &AppCandidate) -> String {
        format!("{} — {}", app.display_name, app.package_id)
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let mut file_rows = Vec::new();
        for kind in kinds(&self.candidates) {
            let apps = apps_for(&self.candidates, &kind);
            let Some(state) = self.shown.get(&kind) else {
                continue;
            };
            if apps.is_empty() {
                continue;
            }
            let options: Vec<String> = apps.iter().map(|a| Self::option(a)).collect();
            let by_option: Vec<(String, String)> = apps
                .iter()
                .map(|app| (Self::option(app), app.package_id.clone()))
                .collect();
            let placeholder = state.get();
            let named = kind.as_str().to_owned();
            let picker = self.cx.bind(
                Dropdown::from_state(options, state)
                    .with_id(&format!("personal-default.{}", kind.as_str()))
                    .with_placeholder(&placeholder),
                effect(DefaultAppsEffect::SET_DEFAULT_APP).map(move |shown: String| {
                    let app = by_option
                        .iter()
                        .find(|(option, _)| *option == shown)
                        .map_or_else(|| shown.clone(), |(_, package)| package.clone());
                    (named.clone(), app)
                }),
            );
            file_rows.push(choice_row_described(
                &Self::resource_label(&kind),
                &t("settings-personal-apps-choice-follows"),
                Box::new(picker),
            ));
        }
        let body = if file_rows.is_empty() {
            rows(vec![caption(&t("settings-personal-apps-empty"))])
        } else {
            rows(file_rows)
        };
        let scope = card(
            &t("settings-personal-apps-scope"),
            rows(vec![
                value_row(
                    &t("settings-personal-apps-applies-to"),
                    &t("settings-personal-apps-applies-value"),
                ),
                caption(&t("settings-personal-apps-scope-caption")),
                Box::new(
                    Button::new(
                        "open-workspace-app-defaults",
                        &t("settings-personal-apps-open-workspace"),
                    )
                    .on_effect(|| {
                        let _ = os_actions::open_tool(os_actions::ToolIntent::WorkspaceSettings {
                            route: os_actions::WorkspaceSettingsRoute::AppsFiles,
                        });
                    }),
                ),
            ]),
        );
        crate::widgets::cards(vec![
            scope,
            card(&t("settings-personal-apps-files-title"), body),
        ])
    }
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
        });
    }
    out.sort_by(|a, b| {
        a.display_name
            .cmp(&b.display_name)
            .then_with(|| a.package_id.cmp(&b.package_id))
    });
    out
}

impl Widget for PersonalAppsPane {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        if self.prefs.refresh() {
            self.show();
        }
        self.composed.sync(state, root);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let composition = self.build_composition();
        let child = self
            .composed
            .build_layout(composition, taffy, theme, node_map);
        crate::panes::pane_node(taffy, child, node_map, self)
    }
    ui::forward_to_composed!(composed);

    fn name(&self) -> &'static str {
        "PersonalAppsPane"
    }
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
        }
    }

    #[test]
    fn personal_apps_copy_comes_from_the_catalogue() {
        assert_eq!(
            t("settings-personal-apps-files-title"),
            "Open files and links with"
        );
        assert_eq!(
            PersonalAppsPane::resource_label(&ResourceKind::extension("pdf").unwrap()),
            "PDF documents"
        );
        assert_eq!(
            PersonalAppsPane::resource_label(&ResourceKind::scheme("https").unwrap()),
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

    /// The pane shows the resolver's pick until the person chooses, then
    /// follows the choice whoever made it.
    #[test]
    fn the_pane_follows_the_choices() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let area = DefaultAppsArea {
            prefs: RefCell::new(cx.follow::<DefaultApps>()),
            candidates: OnceCell::from(vec![
                candidate("so.tilo.a", "pdf"),
                candidate("so.tilo.b", "pdf"),
            ]),
            sample: true,
        };
        let pdf = ResourceKind::extension("pdf").unwrap();
        let mut pane = PersonalAppsPane::new(cx.clone(), &area);
        assert_eq!(pane.shown[&pdf].get(), "so.tilo.a — so.tilo.a");
        cx.settings::<DefaultApps>()
            .update(|apps| {
                apps.handlers.insert(pdf.clone(), "so.tilo.b".into());
            })
            .unwrap();
        let doc = ui::automerge::AutoCommit::new();
        pane.sync(&doc, &ui::automerge::ROOT);
        assert_eq!(pane.shown[&pdf].get(), "so.tilo.b — so.tilo.b");
    }
}

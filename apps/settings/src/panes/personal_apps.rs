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
use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow};
use ui::i18n::ArgValue;
use ui::theme::Theme;
use ui::tilo_actions::ResourcePresentation;
use ui::tilo_resource::{self, Effect};
use ui::widget::{AppModel, Button, Dropdown, ModelPath, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

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
    /// Open the apps and files policy of the caller's workspace.
    #[effect(job, label = "Open workspace apps and files")]
    OpenWorkspaceAppsFiles,
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
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let (kind, package) = match effect {
        DefaultAppsEffect::OpenWorkspaceAppsFiles => {
            if !app.host().is_live() {
                return Err("only a window someone uses opens workspace settings".into());
            }
            return Ok(cx.thread(
                move |_| {
                    os_actions::open_tool(os_actions::ToolIntent::WorkspaceSettings {
                        route: os_actions::WorkspaceSettingsRoute::AppsFiles,
                    })
                },
                |_: &mut SettingsApp, opened: Result<(), String>, _| {
                    opened.map(|()| Effected::done())
                },
            ));
        }
        DefaultAppsEffect::SetDefaultApp { kind, app } => (kind, app),
    };
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
    composition_revision: u64,
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
            composition_revision: 0,
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
                Box::new(self.cx.bind(
                    Button::new(
                        "open-workspace-app-defaults",
                        &t("settings-personal-apps-open-workspace"),
                    ),
                    effect(DefaultAppsEffect::OPEN_WORKSPACE_APPS_FILES),
                )),
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
        let Ok(manifest) = launcher::manifest::read_manifest(&entry.manifest_path) else {
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
            // Chooser placeholders are copied from the current defaults.
            self.composed.clear();
        }
        self.composed.sync(state, root);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let revision = ui::i18n::I18nManager::revision();
        if self.composed.element().is_none() || self.composition_revision != revision {
            let composition = self.build_composition();
            self.composed.replace(composition);
        }
        // Resolving this pane's catalogue can itself update the revision.
        self.composition_revision = ui::i18n::I18nManager::revision();
        // Shared controls sample the live theme on every layout and draw.
        // Keep their hover/press state across ordinary host relayouts.
        let child = if let Some(composition) = self.composed.element_mut() {
            composition.build_layout(taffy, theme, node_map)
        } else {
            // A missing retained child needs a real composition and layout,
            // rather than an empty node or a guessed geometry.
            let composition = self.build_composition();
            self.composed
                .build_layout(composition, taffy, theme, node_map)
        };
        let node = taffy
            .new_with_children(
                taffy::Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::auto(),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.composed.apply_layout(taffy, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.composed
            .draw(scene, transform, text_renderer, theme, presence);
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.composed.handle_event(event, ctx);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.composed.children()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed.children_mut()
    }
    fn name(&self) -> &'static str {
        "PersonalAppsPane"
    }
    fn get_rect(&self) -> Rect {
        self.composed.rect()
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

    #[cfg(unix)]
    fn tool_child(test: &str, accepted: bool) -> bool {
        use std::io::{BufRead, BufReader, Write};
        use std::os::fd::OwnedFd;
        use std::os::unix::net::UnixStream;
        use std::process::{Command, Stdio};
        use std::time::Duration;

        if std::env::var_os("TILO_SETTINGS_APPS_TOOL_CHILD").is_some() {
            return true;
        }
        let (client, mut server) = UnixStream::pair().unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let service = std::thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(server.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: os_actions::ToolRequest = serde_json::from_str(&line).unwrap();
            assert_eq!(request.protocol, os_actions::TOOL_PROTOCOL_VERSION);
            assert_eq!(request.workspace_id, "original-owned-workspace");
            assert_eq!(
                request.intent,
                os_actions::ToolIntent::WorkspaceSettings {
                    route: os_actions::WorkspaceSettingsRoute::AppsFiles,
                }
            );
            std::thread::sleep(Duration::from_millis(300));
            serde_json::to_writer(
                &mut server,
                &os_actions::ToolReply {
                    accepted,
                    message: if accepted {
                        String::new()
                    } else {
                        "original caller refused".into()
                    },
                },
            )
            .unwrap();
            server.write_all(b"\n").unwrap();
        });
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("TILO_SETTINGS_APPS_TOOL_CHILD", "1")
            .env("TILO_WORKSPACE_ID", "original-owned-workspace")
            .env("TILO_SVC_TOOLS_FD", "0")
            .stdin(Stdio::from(OwnedFd::from(client)))
            .status()
            .unwrap();
        service.join().unwrap();
        assert!(
            status.success(),
            "isolated mounted tool invocation failed: {status}"
        );
        false
    }

    #[cfg(unix)]
    #[test]
    fn mounted_workspace_apps_button_does_not_wait_for_real_tool_ack() {
        if !tool_child(
            "panes::personal_apps::tests::mounted_workspace_apps_button_does_not_wait_for_real_tool_ack",
            true,
        ) {
            return;
        }
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let open = crate::effects::open_pane_address(crate::categories::Category::DefaultApps);
        app.call_on(&open.resource, &open.action, serde_json::json!({}))
            .unwrap();
        app.settle();
        let effect = SettingsEffect::from(DefaultAppsEffect::OpenWorkspaceAppsFiles);
        let address = ui::tilo_actions::ActionAddress::new(
            <Settings as tilo_resource::Resource>::KIND,
            effect.id(),
        );
        assert!(app.harness().control_failures().is_empty());
        assert!(
            app.harness()
                .active_bound_jobs()
                .iter()
                .all(|(_, target)| target.address != address)
        );
        let started = std::time::Instant::now();
        app.harness()
            .click_named("open-workspace-app-defaults")
            .unwrap();
        app.settle();
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(150),
            "mounted Apps & Files blocked on the actual tool ACK: {elapsed:?}"
        );
        let jobs: Vec<_> = app
            .harness()
            .active_bound_jobs()
            .into_iter()
            .filter(|(_, target)| target.address == address)
            .collect();
        assert_eq!(jobs.len(), 1, "the real click starts exactly one tool job");
        let invocation = jobs.first().unwrap().0;
        // Retain the original caller until this pointer-originated job observes
        // its actual service reply, rather than closing after a guessed delay.
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(3), |h| {
                    h.active_bound_jobs()
                        .iter()
                        .all(|(active, _)| *active != invocation)
                }),
            "the clicked tool job must observe its actual ACK within the contract"
        );
        assert!(
            app.harness().control_failures().is_empty(),
            "the clicked tool job must complete without a bound-control failure"
        );
    }

    #[cfg(unix)]
    #[test]
    fn mounted_workspace_apps_button_survives_relayout_between_press_and_release() {
        if !tool_child(
            "panes::personal_apps::tests::mounted_workspace_apps_button_survives_relayout_between_press_and_release",
            true,
        ) {
            return;
        }
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let open = crate::effects::open_pane_address(crate::categories::Category::DefaultApps);
        app.call_on(&open.resource, &open.action, serde_json::json!({}))
            .unwrap();
        app.settle();
        let effect = SettingsEffect::from(DefaultAppsEffect::OpenWorkspaceAppsFiles);
        let address = ui::tilo_actions::ActionAddress::new(
            <Settings as tilo_resource::Resource>::KIND,
            effect.id(),
        );
        assert!(app.harness().control_failures().is_empty());
        assert!(
            app.harness()
                .active_bound_jobs()
                .iter()
                .all(|(_, target)| target.address != address)
        );
        let started = std::time::Instant::now();
        let target = app.harness().targets()["open-workspace-app-defaults"];
        let center = target.center();
        app.harness().send(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(center.x, center.y),
        });
        app.harness().send(&WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: winit::event::ElementState::Pressed,
            button: winit::event::MouseButton::Left,
        });
        // A host layout between normal pointer events must not replace the
        // shared button and discard the gesture it is already handling.
        app.harness().request_layout();
        app.harness().relayout();
        app.harness().send(&WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: winit::event::ElementState::Released,
            button: winit::event::MouseButton::Left,
        });
        app.settle();
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(150),
            "mounted Apps & Files blocked on the actual tool ACK: {elapsed:?}"
        );
        let jobs: Vec<_> = app
            .harness()
            .active_bound_jobs()
            .into_iter()
            .filter(|(_, target)| target.address == address)
            .collect();
        assert_eq!(jobs.len(), 1, "the real click starts exactly one tool job");
        let invocation = jobs.first().unwrap().0;
        // Retain the original caller until this pointer-originated job observes
        // its actual service reply, rather than closing after a guessed delay.
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(3), |h| {
                    h.active_bound_jobs()
                        .iter()
                        .all(|(active, _)| *active != invocation)
                }),
            "the clicked tool job must observe its actual ACK within the contract"
        );
        assert!(
            app.harness().control_failures().is_empty(),
            "the clicked tool job must complete without a bound-control failure"
        );
    }

    #[cfg(unix)]
    fn published_workspace_tool(accepted: bool, test: &str) {
        use ui::tilo_actions::InvocationUpdate;
        if !tool_child(test, accepted) {
            return;
        }
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let open = crate::effects::open_pane_address(crate::categories::Category::DefaultApps);
        app.call_on(&open.resource, &open.action, serde_json::json!({}))
            .unwrap();
        app.settle();
        let effect = SettingsEffect::from(DefaultAppsEffect::OpenWorkspaceAppsFiles);
        let started = std::time::Instant::now();
        let invocation = app.start_effect(&effect).unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(150),
            "published Apps & Files blocked on the actual tool ACK: {elapsed:?}"
        );
        assert!(
            !app.harness()
                .job_updates(invocation)
                .iter()
                .any(InvocationUpdate::is_terminal),
            "the original published caller must wait for its actual tool ACK"
        );
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(3), |h| {
                    h.job_updates(invocation)
                        .iter()
                        .any(InvocationUpdate::is_terminal)
                })
        );
        let updates = app.harness().job_updates(invocation);
        assert_eq!(
            updates.iter().filter(|update| update.is_terminal()).count(),
            1
        );
        match (accepted, updates.last()) {
            (
                true,
                Some(InvocationUpdate::Succeeded {
                    invocation: actual,
                    output,
                    ..
                }),
            ) => {
                assert_eq!(*actual, invocation);
                assert!(output.value.is_none());
            }
            (
                false,
                Some(InvocationUpdate::Failed {
                    invocation: actual,
                    error,
                    ..
                }),
            ) => {
                assert_eq!(*actual, invocation);
                assert_eq!(error.message, "original caller refused");
            }
            _ => panic!("the original published caller must receive its actual tool outcome"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn published_workspace_apps_job_waits_for_real_tool_ack() {
        published_workspace_tool(
            true,
            "panes::personal_apps::tests::published_workspace_apps_job_waits_for_real_tool_ack",
        );
    }

    #[cfg(unix)]
    #[test]
    fn published_workspace_apps_job_preserves_real_tool_refusal() {
        published_workspace_tool(
            false,
            "panes::personal_apps::tests::published_workspace_apps_job_preserves_real_tool_refusal",
        );
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

    fn layout_pane(pane: &mut PersonalAppsPane, theme: &Theme) {
        pane.build_layout(&mut TaffyTree::new(), theme, &mut HashMap::new());
    }

    fn presented_choice(widget: &dyn Widget) -> Option<String> {
        if let Some(picker) = widget
            .as_any()
            .and_then(|value| value.downcast_ref::<Dropdown>())
        {
            return Some(picker.selected().to_owned());
        }
        widget.children().into_iter().find_map(presented_choice)
    }

    fn presented_names(widget: &dyn Widget, names: &mut Vec<String>) {
        if let Some(node) = widget.accessibility()
            && let Some(name) = node.name
        {
            names.push(name);
        }
        for child in widget.children() {
            presented_names(child, names);
        }
    }

    #[test]
    fn personal_apps_retained_copy_follows_translation_changes() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let area = DefaultAppsArea {
            prefs: RefCell::new(cx.follow::<DefaultApps>()),
            candidates: OnceCell::from(vec![candidate("so.tilo.a", "pdf")]),
            sample: true,
        };
        let mut pane = PersonalAppsPane::new(cx, &area);
        layout_pane(&mut pane, &Theme::dark());
        let label = t("settings-personal-apps-open-workspace");
        let mut names = Vec::new();
        presented_names(&pane, &mut names);
        assert!(names.contains(&label));
        ui::i18n::I18nManager::set_locale("de-DE");
        layout_pane(&mut pane, &Theme::light().with_ui_scale(1.5));
        let translated = t("settings-personal-apps-open-workspace");
        let mut translated_names = Vec::new();
        presented_names(&pane, &mut translated_names);
        ui::i18n::I18nManager::set_locale("en-US");
        assert_ne!(translated, label);
        assert!(translated_names.contains(&translated));
        assert!(!translated_names.contains(&label));
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
        layout_pane(&mut pane, &Theme::dark());
        pane.sync(&ui::automerge::AutoCommit::new(), &ui::automerge::ROOT);
        assert_eq!(
            presented_choice(&pane).as_deref(),
            Some("so.tilo.a — so.tilo.a")
        );
        cx.settings::<DefaultApps>()
            .update(|apps| {
                apps.handlers.insert(pdf.clone(), "so.tilo.b".into());
            })
            .unwrap();
        let doc = ui::automerge::AutoCommit::new();
        pane.sync(&doc, &ui::automerge::ROOT);
        assert_eq!(pane.shown[&pdf].get(), "so.tilo.b — so.tilo.b");
        layout_pane(&mut pane, &Theme::light());
        pane.sync(&doc, &ui::automerge::ROOT);
        assert_eq!(
            presented_choice(&pane).as_deref(),
            Some("so.tilo.b — so.tilo.b")
        );
    }
}

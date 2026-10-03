//! Personal default applications. Workspace policy deliberately does not live
//! here: it is contextual and belongs to the shell's Workspace Settings.

use std::collections::HashMap;
use ui::doc::read_str;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, transaction::Transactable};
use desktop_policy::{AppCandidate, DefaultAppResolver, PersonalDefaults, ResourceKind};
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{Button, Dropdown, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::actions::{SETTINGS_ACTION_TYPE, SettingsAction, set_text_action};
use crate::i18n::{t, t_args};
use crate::widgets::{caption, card, choice_row_described, rows, value_row};
use ui::i18n::ArgValue;

pub struct PersonalAppsPane {
    candidates: Vec<AppCandidate>,
    defaults: PersonalDefaults,
    /// Where choices persist; `None` is the per-user `default-apps.json`.
    defaults_path: Option<std::path::PathBuf>,
    /// The choice each dropdown last showed, as seeded or read back. The
    /// resolver's pick seeded into an untouched dropdown is not a choice;
    /// only a value that moves away from what was shown is.
    shown: HashMap<String, String>,
    needs_seed: bool,
    commit_context_pending: bool,
    actions: crate::resource::ActionSink,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl PersonalAppsPane {
    pub fn new() -> Self {
        Self::with_state(installed_candidates(), PersonalDefaults::load(), None)
    }

    pub fn with_state(
        candidates: Vec<AppCandidate>,
        defaults: PersonalDefaults,
        defaults_path: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            candidates,
            defaults,
            defaults_path,
            shown: HashMap::new(),
            needs_seed: true,
            commit_context_pending: false,
            actions: crate::resource::ActionSink::default(),
            composed: None,
            rect: Rect::ZERO,
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

    fn resources(&self) -> Vec<ResourceKind> {
        let mut resources: Vec<_> = self
            .candidates
            .iter()
            .flat_map(|a| a.resources.iter().cloned())
            .collect();
        resources.sort();
        resources.dedup();
        resources
    }

    fn apps_for(&self, resource: &ResourceKind) -> Vec<&AppCandidate> {
        let mut apps: Vec<_> = self
            .candidates
            .iter()
            .filter(|a| a.handles(resource))
            .collect();
        apps.sort_by(|a, b| {
            a.display_name
                .cmp(&b.display_name)
                .then_with(|| a.package_id.cmp(&b.package_id))
        });
        apps
    }

    fn option(app: &AppCandidate) -> String {
        format!("{} — {}", app.display_name, app.package_id)
    }

    fn key(resource: &ResourceKind) -> String {
        format!("default_apps.{}", resource.as_str().replace(':', "."))
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let resources = self.resources();
        let mut file_rows = Vec::new();
        for resource in &resources {
            let apps = self.apps_for(resource);
            if apps.is_empty() {
                continue;
            }
            let options: Vec<String> = apps.iter().map(|a| Self::option(a)).collect();
            let current = self
                .defaults
                .handlers
                .get(resource)
                .and_then(|id| apps.iter().find(|a| &a.package_id == id))
                .map_or_else(
                    || options.first().cloned().unwrap_or_default(),
                    |a| Self::option(a),
                );
            let label = Self::resource_label(resource);
            let picker_path = Self::draft_key(&Self::key(resource));
            let picker = Dropdown::new(options, &picker_path)
                .with_id(&format!("personal-default.{}", resource.as_str()))
                .with_placeholder(&current);
            file_rows.push(choice_row_described(
                &label,
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
                        None,
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

    fn draft_key(canonical: &str) -> String {
        format!("{canonical}.edit")
    }

    fn dispatch(&mut self, state: &mut AutoCommit, action: &SettingsAction) {
        self.actions.dispatch_with_context(
            state,
            SETTINGS_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.settings", "Settings", action),
        );
    }

    pub(crate) fn bind_dispatcher(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Settings>,
    ) {
        self.actions.attach(dispatcher);
    }
}

impl Default for PersonalAppsPane {
    fn default() -> Self {
        Self::new()
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(PersonalAppsPane::new())
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
    ui::provides!(dyn ui::undo::UndoPoints);

    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
        if self.needs_seed {
            let initial_values = {
                let resolver = DefaultAppResolver::new(&self.candidates);
                let mut initial_values = Vec::new();
                for resource in self.resources() {
                    if let Some(selected) = resolver.resolve(&resource, "", &self.defaults, None)
                        && let Some(app) = self
                            .candidates
                            .iter()
                            .find(|a| a.package_id == selected.package_id)
                    {
                        initial_values.push((Self::key(&resource), Self::option(app)));
                    }
                }
                initial_values
            };
            for (key, value) in initial_values {
                self.dispatch(state, &set_text_action(&key, &value));
                self.shown.insert(key, value);
            }
            self.needs_seed = false;
        }
        let mut changed = false;
        for resource in self.resources() {
            let key = Self::key(&resource);
            let canonical = read_str(state, root, &key);
            let draft_key = Self::draft_key(&key);
            let draft = read_str(state, root, &draft_key);
            let Some(option) = canonical.clone() else {
                continue;
            };
            if canonical.as_deref()
                == self
                    .defaults
                    .handlers
                    .get(&resource)
                    .and_then(|id| {
                        self.apps_for(&resource)
                            .into_iter()
                            .find(|app| &app.package_id == id)
                    })
                    .map(Self::option)
                    .as_deref()
                && draft.as_deref().is_some_and(|draft| draft != option)
            {
                self.dispatch(state, &set_text_action(&key, draft.unwrap_or_default()));
                continue;
            }
            if let Some(canonical) = canonical
                && read_str(state, root, &draft_key).as_deref() != Some(&canonical)
            {
                let _ = state.put(root, draft_key, canonical);
            }
            if self.shown.get(&key) == Some(&option) {
                continue;
            }
            self.shown.insert(key.clone(), option.clone());
            let Some(app) = self
                .apps_for(&resource)
                .into_iter()
                .find(|a| Self::option(a) == option)
            else {
                continue;
            };
            if self.defaults.handlers.get(&resource) != Some(&app.package_id) {
                let resolver = DefaultAppResolver::new(&self.candidates);
                if self
                    .defaults
                    .set(resource, app.package_id.clone(), &resolver)
                    .is_ok()
                {
                    changed = true;
                }
            }
        }
        if changed {
            let _ = match &self.defaults_path {
                Some(path) => self.defaults.save_at(path),
                None => self.defaults.save(),
            };
        }
        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
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
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x + l.size.width),
            origin.y + f64::from(l.location.y + l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let was_handled = *ctx.handled;
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
        self.commit_context_pending |= !was_handled && *ctx.handled;
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }
    fn name(&self) -> &'static str {
        "PersonalAppsPane"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::undo::UndoPoints for PersonalAppsPane {
    fn take_commit_context(&mut self) -> Option<ui::tilo_actions::ActionContext> {
        let pending = std::mem::take(&mut self.commit_context_pending)
            || self
                .composed
                .as_ref()
                .is_some_and(|content| ui::undo::needs_commit(content.as_ref()));
        crate::panes::preference_commit_context(
            pending,
            "personal-apps",
            &t("settings-personal-apps-undo-name"),
            &t("settings-personal-apps-undo-action"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            ".pdf files"
        );
        assert_eq!(
            PersonalAppsPane::resource_label(&ResourceKind::scheme("https").unwrap()),
            "https: links"
        );
    }

    /// Dropdown choices persist and MIME/scheme claims are personal defaults.
    #[test]
    fn personal_default_apps_persist_and_expose_claims() {
        // Case `dropdown_change_persists_personal_choice`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("default-apps.json");
            let candidates = vec![candidate("so.tilo.a", "pdf"), candidate("so.tilo.b", "pdf")];
            let mut pane = PersonalAppsPane::with_state(
                candidates,
                PersonalDefaults::default(),
                Some(path.clone()),
            );
            let mut doc = AutoCommit::new();
            pane.sync(&mut doc, &automerge::ROOT);
            doc.put(
                &automerge::ROOT,
                "default_apps.ext.pdf",
                "so.tilo.b — so.tilo.b",
            )
            .unwrap();
            pane.sync(&mut doc, &automerge::ROOT);
            assert_eq!(
                pane.defaults
                    .handlers
                    .get(&ResourceKind::extension("pdf").unwrap())
                    .map(String::as_str),
                Some("so.tilo.b")
            );
            assert_eq!(
                PersonalDefaults::load_at(&path)
                    .handlers
                    .get(&ResourceKind::extension("pdf").unwrap())
                    .map(String::as_str),
                Some("so.tilo.b"),
                "the choice survives a reload"
            );
        }

        // Case `mime_and_scheme_claims_are_exposed_as_personal_defaults`.
        {
            let candidate = AppCandidate {
                package_id: "so.tilo.browser".into(),
                display_name: "Browser".into(),
                resources: [
                    ResourceKind::mime("text/html").unwrap(),
                    ResourceKind::scheme("https").unwrap(),
                ]
                .into(),
            };
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("default-apps.json");
            let mut pane = PersonalAppsPane::with_state(
                vec![candidate],
                PersonalDefaults::default(),
                Some(path.clone()),
            );
            let mut doc = AutoCommit::new();
            pane.sync(&mut doc, &automerge::ROOT);
            pane.sync(&mut doc, &automerge::ROOT);
            assert!(
                pane.defaults.handlers.is_empty(),
                "showing the resolver's pick is not a personal choice"
            );
            assert!(!path.exists(), "opening the pane persists nothing");
            assert_eq!(
                read_str(&doc, &automerge::ROOT, "default_apps.mime.text/html").as_deref(),
                Some("Browser — so.tilo.browser")
            );
            assert_eq!(
                read_str(&doc, &automerge::ROOT, "default_apps.scheme.https").as_deref(),
                Some("Browser — so.tilo.browser")
            );
        }
    }
}

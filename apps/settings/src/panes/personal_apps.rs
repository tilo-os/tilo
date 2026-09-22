//! Personal default applications. Workspace policy deliberately does not live
//! here: it is contextual and belongs to the shell's Workspace Settings.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use desktop_policy::{AppCandidate, DefaultAppResolver, PersonalDefaults, ResourceKind};
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{Button, Dropdown, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::actions::{SETTINGS_ACTION_TYPE, SettingsAction, set_text_action};
use crate::widgets::{caption, card, choice_row_described, rows, value_row};

pub struct PersonalAppsPane {
    candidates: Vec<AppCandidate>,
    defaults: PersonalDefaults,
    needs_seed: bool,
    commit_context_pending: bool,
    actions: ui::ActionHost,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl PersonalAppsPane {
    pub fn new() -> Self {
        Self::with_state(installed_candidates(), PersonalDefaults::load())
    }

    pub fn with_state(candidates: Vec<AppCandidate>, defaults: PersonalDefaults) -> Self {
        Self {
            candidates,
            defaults,
            needs_seed: true,
            commit_context_pending: false,
            actions: ui::ActionHost::new(),
            composed: None,
            rect: Rect::ZERO,
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
            let label = resource.display_label();
            let picker_path = Self::draft_key(&Self::key(resource));
            let picker = Dropdown::new(options, &picker_path)
                .with_id(&format!("personal-default.{}", resource.as_str()))
                .with_placeholder(&current);
            file_rows.push(choice_row_described(
                &label,
                "Your choice follows you unless this workspace chooses another app.",
                Box::new(picker),
            ));
        }
        let body = if file_rows.is_empty() {
            rows(vec![caption(
                "No installed application currently declares a supported file or link type. Defaults appear here as apps are installed.",
            )])
        } else {
            rows(file_rows)
        };
        let scope = card(
            "Personal",
            rows(vec![
                value_row("Applies to", "Your identity on every device"),
                caption(
                    "A workspace may set a shared choice. In Workspace Settings you can follow it, return to this personal choice, or select a different app for that workspace.",
                ),
                Box::new(
                    Button::new(
                        "open-workspace-app-defaults",
                        "Open current workspace app rules",
                        None,
                    )
                    .with_size(236.0, 40.0)
                    .on_effect(|| {
                        let _ = os_actions::open_tool(os_actions::ToolIntent::WorkspaceSettings {
                            route: os_actions::WorkspaceSettingsRoute::AppsFiles,
                        });
                    }),
                ),
            ]),
        );
        crate::widgets::cards(vec![scope, card("Open files and links with", body)])
    }

    fn read_str(state: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
        match state.get(root, key) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                automerge::ScalarValue::Str(text) => Some(text.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    fn draft_key(canonical: &str) -> String {
        format!("{canonical}.edit")
    }

    fn dispatch(&mut self, state: &mut AutoCommit, action: &SettingsAction) {
        self.actions.dispatch_with_context(
            state,
            SETTINGS_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("org.tilo.Settings", "Settings", action),
        );
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
    for entry in launcher::registry::list_installed().unwrap_or_default() {
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
                .unwrap_or_else(|| launcher::registry::display_name_for_package_id(&package_id)),
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
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
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
                self.dispatch(state, &set_text_action(key, value));
            }
            self.needs_seed = false;
        }
        let mut changed = false;
        for resource in self.resources() {
            let key = Self::key(&resource);
            let canonical = Self::read_str(state, root, &key);
            let draft_key = Self::draft_key(&key);
            let draft = Self::read_str(state, root, &draft_key);
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
                && Self::read_str(state, root, &draft_key).as_deref() != Some(&canonical)
            {
                let _ = state.put(root, draft_key, canonical);
            }
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
            let _ = self.defaults.save();
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
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let was_handled = *handled;
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                cn,
                taffy,
            );
        }
        self.commit_context_pending |= !was_handled && *handled;
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
    fn take_commit_context(&mut self) -> Option<ui::tilo_actions::ActionContext> {
        let pending = std::mem::take(&mut self.commit_context_pending)
            || self
                .composed
                .as_ref()
                .is_some_and(|content| content.needs_commit());
        crate::panes::preference_commit_context(
            pending,
            "personal-apps",
            "Personal app defaults",
            "Change default apps",
        )
    }
    fn name(&self) -> &'static str {
        "PersonalAppsPane"
    }
    fn get_rect(&self) -> Rect {
        self.rect
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
    fn dropdown_change_persists_personal_choice() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new("personal_default");
        let candidates = vec![candidate("so.tilo.a", "pdf"), candidate("so.tilo.b", "pdf")];
        let mut pane = PersonalAppsPane::with_state(candidates, PersonalDefaults::default());
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
    }

    #[test]
    fn mime_and_scheme_claims_are_exposed_as_personal_defaults() {
        let candidate = AppCandidate {
            package_id: "so.tilo.browser".into(),
            display_name: "Browser".into(),
            resources: [
                ResourceKind::mime("text/html").unwrap(),
                ResourceKind::scheme("https").unwrap(),
            ]
            .into(),
        };
        let mut pane = PersonalAppsPane::with_state(vec![candidate], PersonalDefaults::default());
        let mut doc = AutoCommit::new();
        pane.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            PersonalAppsPane::read_str(&doc, &automerge::ROOT, "default_apps.mime.text/html")
                .as_deref(),
            Some("Browser — so.tilo.browser")
        );
        assert_eq!(
            PersonalAppsPane::read_str(&doc, &automerge::ROOT, "default_apps.scheme.https")
                .as_deref(),
            Some("Browser — so.tilo.browser")
        );
    }
}

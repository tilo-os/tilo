//! A resource-rooted standalone host for the same administration panel.
use super::{PanelBindings, ProviderEffect, ProviderPanel};
use agent_provider_config::ProviderConfig;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use ui::framework::{
    ActionControl, Actions, EffectAnswer, EffectCx, Effected, LiveRow, Publishes, ResourceApp,
    ViewCx,
};
use ui::models::ProviderAdministration;
use ui::tilo_resource::{self, Binds, Model, resource};
use ui::widget::{AppModel, Composed, Element, ModelPath, TaffyTreeExt, Widget};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Providers {}
#[resource(kind = "model-providers.root", label = "Model providers")]
impl Providers {}

thread_local! {
    static LAUNCHER_ADMIN: std::cell::RefCell<ui::models::ProviderAdministration> =
        std::cell::RefCell::default();
}

/// Standalone administration app. Model selection is deliberately not required.
#[derive(Default)]
pub struct ProviderSettingsApp;
impl ProviderSettingsApp {
    /// Capture the native launcher's explicit grant before mounting.
    pub fn init(&mut self) {
        LAUNCHER_ADMIN.with(|slot| {
            *slot.borrow_mut() = ProviderAdministration::from_launcher().unwrap_or_default();
        });
    }
}
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(ProviderSettingsApp)
}
impl ResourceApp for ProviderSettingsApp {
    type Resource = Providers;
    const ID: &'static str = "so.tilo.model-providers";
    const TITLE: &'static str = "Model providers";
    const ICON: Option<&'static str> = Some("settings");
    fn view(cx: &mut ViewCx<Providers>) -> Element {
        let window = ProviderSurface::attached(cx);
        let rows = Rc::clone(&window.rows);
        cx.window_effects(ProviderSurface::effect);
        cx.publishing(
            Publishes::new().live_rows(move || rows.borrow().clone()),
            Box::new(window),
        )
    }
}
#[derive(Clone, Default)]
pub(super) struct Bindings {
    actions: Option<Actions<Providers>>,
}
impl PanelBindings for Bindings {
    fn bind<I: 'static, C: ActionControl<I>>(
        &self,
        control: C,
        binding: impl Binds<ProviderEffect, I>,
    ) -> C {
        match &self.actions {
            Some(actions) => actions.bind_effect(control, binding),
            None => control.unavailable(),
        }
    }
    fn bind_invocation<C: ActionControl<()>>(&self, control: C, effect: ProviderEffect) -> C {
        match &self.actions {
            Some(actions) => actions.bind_effect_invocation(control, &effect),
            None => control.unavailable(),
        }
    }
}
pub(super) struct ProviderSurface {
    pub(super) panel: ProviderPanel<Bindings>,
    composed: Composed,
    offset: Rc<std::cell::Cell<(f32, f32)>>,
    /// The panel's provider rows as of its last change, which the view
    /// publishes.
    rows: Rc<RefCell<Vec<LiveRow>>>,
}
impl ProviderSurface {
    /// The panel over the administration the launcher granted, a host lent,
    /// or a live window reaches on its own, binding its controls to `cx`.
    fn attached(cx: &ViewCx<Providers>) -> Self {
        let mut administration =
            LAUNCHER_ADMIN.with(|slot| std::mem::take(&mut *slot.borrow_mut()));
        let mut lent = false;
        while let Some(loan) = cx.take_lent::<ProviderAdministration>() {
            administration = loan;
            lent = true;
        }
        if !lent && !administration.is_available() && cx.mode().is_live() {
            administration = ProviderAdministration::from_launcher().unwrap_or_default();
        }
        let window = Self {
            panel: ProviderPanel::new(
                Bindings {
                    actions: Some(cx.actions()),
                },
                administration,
                ProviderConfig::default(),
            ),
            composed: Composed::default(),
            offset: Rc::default(),
            rows: Rc::default(),
        };
        window.publish();
        window
    }
    /// Keep the panel's rows for the view's publication.
    fn publish(&self) {
        *self.rows.borrow_mut() = self.panel.rows();
    }
    fn effect(&mut self, effect: ProviderEffect, cx: &mut EffectCx<Providers>) -> EffectAnswer {
        if matches!(effect, ProviderEffect::ListPresets) {
            // Sign-ins run where a host offers them; this window offers keys.
            return Ok(Effected::value(crate::presets_listing(false)));
        }
        if let ProviderEffect::InspectModelRequestLimits { provider, model } = effect {
            return self
                .panel
                .inspect_limits(&provider, &model)
                .map(Effected::value);
        }
        self.panel.submit_provider(effect, cx.reply())?;
        self.publish();
        Ok(Effected::Pending)
    }
}
impl Widget for ProviderSurface {
    ui::forward_to_composed!(composed);
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.panel.poll_admin();
        if std::mem::take(&mut self.panel.changed) {
            self.composed.invalidate();
            self.publish();
        }
        self.composed.sync(state, path);
    }
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let panel = &self.panel;
        let offset = &self.offset;
        let child = self.composed.build_layout_kept(
            || -> Box<dyn Widget> {
                if panel.administered_elsewhere() {
                    return Box::new(
                        ui::widget::EmptyState::new(crate::i18n::t(
                            "agent-provider-admin-only-title",
                        ))
                        .with_icon(ui::icons::Icon::Lock)
                        .with_message(crate::i18n::t("agent-provider-admin-only-message"))
                        .with_id("model-provider-administration"),
                    );
                }
                Box::new(
                    panel
                        .sections()
                        .into_iter()
                        .fold(ui::widget::Page::new(), |page, section| {
                            page.section(Box::new(section.into_section()))
                        })
                        .with_semantic_key("model-provider-administration")
                        .with_persisted_offset(Rc::clone(offset)),
                )
            },
            taffy,
            theme,
            node_map,
        );
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }
    fn needs_animation_frame(&self) -> bool {
        self.panel.admin_pending.is_some()
            || self
                .composed
                .element()
                .is_some_and(Widget::needs_animation_frame)
    }
    fn name(&self) -> &'static str {
        "ProviderAdministration"
    }
}

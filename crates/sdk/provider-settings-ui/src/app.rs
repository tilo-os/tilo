//! A resource-rooted standalone host for the same administration panel.
use super::{PanelBindings, ProviderEffect, ProviderPanel, widgets};
use agent_provider_config::ProviderConfig;
use std::{collections::HashMap, rc::Rc};
use ui::framework::{
    ActionControl, Actions, EffectAnswer, EffectCx, Effected, LiveRow, ResourceApp,
    ResourceSurface, SurfaceCx, ViewCx,
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
            *slot.borrow_mut() =
                ui::models::ProviderAdministration::from_launcher().unwrap_or_default();
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
        cx.surface(ProviderSurface::default())
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
    revision: u64,
    offset: Rc<std::cell::Cell<(f32, f32)>>,
}
impl Default for ProviderSurface {
    fn default() -> Self {
        Self {
            panel: ProviderPanel::new(
                Bindings::default(),
                ProviderAdministration::default(),
                ProviderConfig::default(),
            ),
            composed: Composed::default(),
            revision: 0,
            offset: Rc::default(),
        }
    }
}
impl Widget for ProviderSurface {
    ui::forward_to_composed!(composed);
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.panel.poll_admin();
        if std::mem::take(&mut self.panel.changed)
            || self.revision != ui::i18n::I18nManager::revision()
        {
            self.composed.clear();
            self.composed.invalidate();
        }
        self.composed.sync(state, path);
    }
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let composition = self.composed.take().unwrap_or_else(|| {
            Box::new(
                ui::widget::FormPage::untitled(widgets::form(self.panel.sections()))
                    .with_semantic_key("model-provider-administration")
                    .with_persisted_offset(Rc::clone(&self.offset)),
            )
        });
        self.revision = ui::i18n::I18nManager::revision();
        let child = self
            .composed
            .build_layout(composition, taffy, theme, node_map);
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
impl ResourceSurface<Providers, ProviderEffect> for ProviderSurface {
    fn present(&mut self, _: &Rc<Providers>) {}
    fn attach(&mut self, cx: SurfaceCx<Providers>) {
        let mut administration =
            LAUNCHER_ADMIN.with(|slot| std::mem::take(&mut *slot.borrow_mut()));
        let mut lent = false;
        while let Some(loan) = cx.take_lent::<ui::models::ProviderAdministration>() {
            administration = loan;
            lent = true;
        }
        if !lent && !administration.is_available() && cx.mode().is_live() {
            administration =
                ui::models::ProviderAdministration::from_launcher().unwrap_or_default();
        }
        self.panel = ProviderPanel::new(
            Bindings {
                actions: Some(cx.actions()),
            },
            administration,
            ProviderConfig::default(),
        );
    }
    fn effect(
        &mut self,
        effect: ProviderEffect,
        cx: &mut EffectCx<Providers, Self>,
    ) -> EffectAnswer {
        if let ProviderEffect::InspectModelRequestLimits { provider, model } = effect {
            return self
                .panel
                .inspect_limits(&provider, &model)
                .map(Effected::value);
        }
        self.panel.submit_provider(effect, cx.reply())?;
        Ok(Effected::Pending)
    }
    fn live_rows(&self) -> Vec<LiveRow> {
        self.panel.rows()
    }
}

//! Settings as a [`tilo_resource::Resource`]: the door Settings' effects and
//! live rows are published on.
//!
//! Settings edits no document of its own. The person's and the device's
//! preferences are the session's settings records, which every effect reads
//! and writes through the app's settings document; this device's hardware
//! and services are its own. So the resource holds nothing: it names
//! Settings to the palette, agents and scripts, and every change is a
//! [`SettingsEffect`].

use std::rc::Rc;

use ui::application::ComponentBehaviour;
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Fixture, LiveRow, ResourceApp, ResourceSurface,
    SurfaceCx, ViewCx,
};
use ui::tilo_resource::{Doc, ModelError, NoAction, Resource};
use ui::widget::Element;

use crate::effects::SettingsEffect;
use crate::host::PaneCx;
use crate::settings_app::SettingsApp;

/// The Settings resource: the door its effects and live rows are published
/// on.
pub struct Settings;

impl Resource for Settings {
    const KIND: &'static str = "settings.root";
    const LABEL: &'static str = "Settings";
    const ACTION_TYPE: &'static str = "settings.action";

    type State = ();
    type Action = NoAction;

    fn read(_doc: &Doc) -> Result<(), ModelError> {
        Ok(())
    }
}

/// Framework app: the settings widget is the surface over [`Settings`].
pub struct SettingsShell;

impl ResourceApp for SettingsShell {
    type Resource = Settings;
    const ID: &'static str = "so.tilo.settings";
    const TITLE: &'static str = "Settings";
    const ICON: Option<&'static str> = Some("settings");
    // Sidebar filtering rebuilds the widget tree, and the Appearance pane's
    // scale, theme, contrast and motion apply live.
    const BEHAVIOUR: ComponentBehaviour = ComponentBehaviour {
        autofocus_on_structure_change: true,
        appearance_reconcile: true,
        ..ComponentBehaviour::DEFAULT
    };

    fn view(cx: &mut ViewCx<Settings>) -> Element {
        cx.surface(SettingsApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        crate::scenarios::names()
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        crate::scenarios::viewport(scenario)
    }

    fn fixture(fixture: &mut Fixture<Settings>) {
        crate::scenarios::prepare(fixture);
    }
}

impl ResourceSurface<Settings, SettingsEffect> for SettingsApp {
    fn present(&mut self, _state: &Rc<()>) {}

    fn attach(&mut self, cx: SurfaceCx<Settings>) {
        self.attach_host(PaneCx::attached(&cx));
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scene(crate::scenarios::staged(scenario));
    }

    fn effect(
        &mut self,
        effect: SettingsEffect,
        cx: &mut EffectCx<Settings, Self>,
    ) -> EffectAnswer {
        crate::effects::answer(self, effect, cx)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        crate::effects::statuses(self)
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        crate::effects::rows(self)
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        crate::effects::row(self, id)
    }
}

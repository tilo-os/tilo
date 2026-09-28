//! Settings as a [`tilo_resource::Resource`]: one action vocabulary, one
//! publish path for collaborative preference keys.

use std::rc::Rc;

use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::EncodedAction;
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{SETTINGS_ACTION_TYPE, SettingsAction, register};
use crate::settings_app::SettingsApp;

/// Projection of portable preference keys stored at the document root.
#[derive(Clone, Debug, Default)]
pub struct SettingsState {
    pub keys: Vec<(String, String)>,
}

/// The settings workspace resource.
pub struct Settings;

impl Resource for Settings {
    const KIND: &'static str = "settings.root";
    const LABEL: &'static str = "Settings";
    const ACTION_TYPE: &'static str = SETTINGS_ACTION_TYPE;

    type State = SettingsState;
    type Action = SettingsAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<SettingsState, ModelError> {
        let mut keys = Vec::new();
        for key in doc.keys(&ROOT) {
            let Ok(Some((value, _))) = doc.get(&ROOT, key.as_str()) else {
                continue;
            };
            let rendered = match value {
                Value::Scalar(scalar) => match scalar.as_ref() {
                    ScalarValue::Str(text) => text.to_string(),
                    ScalarValue::Boolean(flag) => flag.to_string(),
                    ScalarValue::Int(n) => n.to_string(),
                    ScalarValue::Uint(n) => n.to_string(),
                    ScalarValue::F64(n) => n.to_string(),
                    _ => continue,
                },
                _ => continue,
            };
            keys.push((key, rendered));
        }
        keys.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(SettingsState { keys })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        // Named setting rows stay prompted ActionSpecs on the published tree
        // (param names and device-local effects). Stub declarations here would
        // shadow those prompts.
        Vec::new()
    }
}

/// Framework app: the settings widget is the surface over [`Settings`].
pub struct SettingsShell;

impl ResourceApp for SettingsShell {
    type Resource = Settings;
    const ID: &'static str = "so.tilo.settings";
    const TITLE: &'static str = "Settings";
    const ICON: Option<&'static str> = Some("settings");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Settings>) -> Element {
        cx.surface(SettingsApp::new())
    }
}

impl ResourceSurface<Settings> for SettingsApp {
    fn present(&mut self, state: &Rc<SettingsState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Settings>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher shared by the preference panes.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Settings>>,
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Settings>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &SettingsAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(crate::actions::encode(action));
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, type_id, action, context),
        }
    }

    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }

    /// Hosted commits are recorded by the dispatcher. Standalone tests still
    /// read the local host's rising edge.
    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }
}

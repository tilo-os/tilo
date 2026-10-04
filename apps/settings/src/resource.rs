//! Settings as a [`tilo_resource::Resource`]: one action vocabulary, one
//! publish path for collaborative preference keys.

use std::rc::Rc;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
use ui::application::ComponentBehaviour;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::EncodedAction;
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    SETTINGS_ACTION_TYPE, SettingValue, SettingsAction, document_children, register,
};
use crate::settings_app::SettingsApp;

/// Projection of portable preference keys stored at the document root.
#[derive(Clone, Debug, Default)]
pub struct SettingsState {
    /// Typed root scalars so publish can rebuild flip targets without losing
    /// bool / number / text distinctions.
    pub values: Vec<(String, SettingValue)>,
}

impl SettingsState {
    /// Project the collaborative preference keys from `doc`.
    pub(crate) fn from_doc(doc: &Doc) -> Self {
        Settings::read(doc).unwrap_or_default()
    }

    /// Rebuild a document carrying only the projected preference keys.
    pub(crate) fn to_doc(&self) -> AutoCommit {
        let mut doc = AutoCommit::new();
        for (key, value) in &self.values {
            let _ = doc.put(&ROOT, key.as_str(), value.scalar());
        }
        doc
    }
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
        let mut values = Vec::new();
        for key in doc.keys(&ROOT) {
            let Ok(Some((value, _))) = doc.get(&ROOT, key.as_str()) else {
                continue;
            };
            let typed = match value {
                Value::Scalar(scalar) => match scalar.as_ref() {
                    ScalarValue::Str(text) => SettingValue::Text(text.to_string()),
                    ScalarValue::Boolean(flag) => SettingValue::Bool(*flag),
                    ScalarValue::Int(n) => SettingValue::Int(*n),
                    ScalarValue::Uint(n) => {
                        SettingValue::Int(i64::try_from(*n).unwrap_or(i64::MAX))
                    }
                    ScalarValue::F64(n) => SettingValue::Float(*n),
                    _ => continue,
                },
                Value::Object(_) => continue,
            };
            values.push((key, typed));
        }
        values.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(SettingsState { values })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        // Named setting rows are prompted ActionSpecs on section children
        // (param names, flip targets). Stub declarations here would shadow
        // those prompts — see [`document_children`].
        Vec::new()
    }

    fn children(state: &SettingsState) -> Vec<tilo_actions::Resource> {
        document_children(state)
    }
}

/// Framework app: the settings widget is the surface over [`Settings`].
pub struct SettingsShell;

impl ResourceApp for SettingsShell {
    type Resource = Settings;
    const ID: &'static str = "so.tilo.settings";
    const TITLE: &'static str = "Settings";
    const ICON: Option<&'static str> = Some("settings");
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }
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
}

impl ResourceSurface<Settings> for SettingsApp {
    fn present(&mut self, state: &Rc<SettingsState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Settings>) {
        self.attach_framework(cx.dispatcher());
    }

    // Preference and device rows publish their document edits as hand-built
    // rows of the widget's own rather than through the resource.
    fn publishes_document_actions(&self) -> bool {
        true
    }
}

/// Framework dispatcher shared by the preference panes.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Settings>>,
    pub(crate) last: Option<EncodedAction>,
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

    #[allow(dead_code, reason = "framework migration test helper")]
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

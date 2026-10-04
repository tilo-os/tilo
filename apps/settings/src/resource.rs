//! Settings as a [`tilo_resource::Resource`]: one action vocabulary, one
//! publish path for collaborative preference keys.

use std::rc::Rc;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
use ui::application::ComponentBehaviour;
use ui::framework::{
    Dispatcher, Effect, EffectAnswer, EffectCx, Effected, LaunchInput, ResourceApp,
    ResourceSurface, SurfaceCx, ViewCx,
};
use ui::tilo_actions::EncodedAction;
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, Resource};
// The `Effect` derive names the runtime crate by this path.
use ui::tilo_resource;
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

/// What Settings shows when a link or another app asks: a pane, or one
/// local model's card. Showing changes no setting and installs nothing.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "settings.effect")]
pub enum SettingsEffect {
    /// Show one Settings pane.
    #[effect(view, label = "Show settings pane")]
    ShowPane {
        #[param("Pane id, for example agent_models or permissions")]
        pane: String,
    },
    /// Show one signed local model in Agent Models, opened at its licence
    /// confirmation when it is not installed yet.
    #[effect(view, label = "Show local model")]
    ShowLocalModel {
        #[param("Signed model id, for example u2net-subject")]
        id: String,
    },
}

/// Decode a `settings://` link: `settings://models/<model-id>` shows that
/// local model's card and `settings://<pane>[/…]` shows a pane. The launcher
/// already checked the scheme; this keeps a malformed value from becoming a
/// model or pane id.
///
/// # Errors
/// Returns why `uri` is not a Settings link this version understands.
pub fn parse_settings_uri(uri: &str) -> Result<SettingsEffect, String> {
    let rest = uri
        .strip_prefix("settings://")
        .or_else(|| uri.strip_prefix("settings:"))
        .ok_or_else(|| "not a Settings link".to_string())?;
    if let Some(id) = rest.strip_prefix("models/") {
        let id = id.trim_end_matches('/');
        if id.is_empty()
            || id.len() > 128
            || !id.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.' | b'/')
            })
        {
            return Err("invalid local model id in Settings link".into());
        }
        return Ok(SettingsEffect::ShowLocalModel { id: id.to_owned() });
    }
    let pane = rest.split('/').next().unwrap_or_default();
    // Camera and other apps link to the privacy controls by their common
    // name; the pane is Permissions.
    let pane = match pane {
        "privacy" => "permissions",
        "models" => "agent_models",
        other => other,
    };
    crate::categories::Category::ALL
        .iter()
        .find(|category| category.slug() == pane)
        .map(|category| SettingsEffect::ShowPane {
            pane: category.slug().to_owned(),
        })
        .ok_or_else(|| "unknown Settings pane in link".into())
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
    /// A `settings://` link.
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Settings>) -> Element {
        cx.surface(SettingsApp::new())
    }

    /// `settings://models/<id>` opens Agent Models at that model's card and
    /// `settings://<pane>` opens the pane. Neither changes anything.
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Settings>> {
        let Some(argument) = input.argument() else {
            return Vec::new();
        };
        match parse_settings_uri(argument) {
            Ok(effect) => vec![Invoke::effect(&effect)],
            Err(error) => {
                tracing::warn!(%error, "ignoring the Settings link");
                Vec::new()
            }
        }
    }
}

impl ResourceSurface<Settings, SettingsEffect> for SettingsApp {
    fn effect(
        &mut self,
        effect: SettingsEffect,
        _cx: &mut EffectCx<Settings, Self>,
    ) -> EffectAnswer {
        self.show(effect)?;
        Ok(Effected::done())
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Model links name a validated model id, pane links a known pane, and
    /// anything else is refused rather than guessed.
    #[test]
    fn settings_links_decode_to_what_they_show() {
        assert_eq!(
            parse_settings_uri("settings://models/u2net-subject"),
            Ok(SettingsEffect::ShowLocalModel {
                id: "u2net-subject".into()
            })
        );
        assert_eq!(
            parse_settings_uri("settings://privacy/camera"),
            Ok(SettingsEffect::ShowPane {
                pane: "permissions".into()
            })
        );
        assert_eq!(
            parse_settings_uri("settings://agent_models"),
            Ok(SettingsEffect::ShowPane {
                pane: "agent_models".into()
            })
        );
        assert!(parse_settings_uri("settings://models/").is_err());
        assert!(parse_settings_uri("settings://models/U2NET?x=1").is_err());
        assert!(parse_settings_uri("settings://nowhere").is_err());
        assert!(parse_settings_uri("https://models/u2net-subject").is_err());
    }
}

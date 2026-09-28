//! On-screen keyboard as a [`tilo_resource::Resource`]: preference keys in the
//! shared document, with [`crate::actions::SetPreference`] as the replayable
//! action.

use automerge::{ROOT, ReadDoc, ScalarValue, Value};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{SetPreference, TYPE_ID, register};
use crate::framework_history::{LAYOUT, PERSONALIZATION, SUGGESTIONS};
use crate::widget::OskWidget;

/// Preference projection from the keyboard's Automerge document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OskPreferences {
    pub layout: Option<String>,
    pub suggestions: Option<bool>,
    pub personalization: Option<bool>,
}

/// The on-screen keyboard resource.
pub struct Osk;

impl Resource for Osk {
    const KIND: &'static str = "osk.keyboard";
    const LABEL: &'static str = "On-screen keyboard";
    const ACTION_TYPE: &'static str = TYPE_ID;

    type State = OskPreferences;
    type Action = SetPreference;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<OskPreferences, ModelError> {
        Ok(OskPreferences {
            layout: read_str(doc, LAYOUT),
            suggestions: read_bool(doc, SUGGESTIONS),
            personalization: read_bool(doc, PERSONALIZATION),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        // Layout/suggestion affordances stay named ActionSpecs on the
        // published tree (resources() in actions.rs).
        Vec::new()
    }
}

fn read_str(doc: &Doc, key: &str) -> Option<String> {
    match doc.get(ROOT, key).ok().flatten() {
        Some((Value::Scalar(scalar), _)) => match scalar.as_ref() {
            ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn read_bool(doc: &Doc, key: &str) -> Option<bool> {
    match doc.get(ROOT, key).ok().flatten() {
        Some((Value::Scalar(scalar), _)) => match scalar.as_ref() {
            ScalarValue::Boolean(flag) => Some(*flag),
            _ => None,
        },
        _ => None,
    }
}

/// Framework app: the keyboard widget is the surface over [`Osk`].
pub struct OskShell;

impl ResourceApp for OskShell {
    type Resource = Osk;
    const ID: &'static str = "so.tilo.osk";
    const TITLE: &'static str = "On-screen keyboard";
    const ICON: Option<&'static str> = Some("osk");
    const SHOT_VIEWPORT: (u32, u32) = (720, 360);

    fn view(cx: &mut ViewCx<Osk>) -> Element {
        cx.surface(OskWidget::new())
    }
}

impl ResourceSurface<Osk> for OskWidget {
    fn present(&mut self, _state: &std::rc::Rc<OskPreferences>) {}

    fn attach(&mut self, _cx: SurfaceCx<Osk>) {}
}

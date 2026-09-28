//! Themes as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path for the theme library.

use automerge::{AutoCommit, ROOT};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::EncodedAction;
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{THEMES_ACTION_TYPE, ThemesAction, register};
use crate::app::ThemesApp;
use crate::doc::{self, KEY_ACTIVE};
use crate::model::ThemeSpec;

/// Catalog plus the active theme id.
#[derive(Clone, Debug, Default)]
pub struct ThemesState {
    pub catalog: Vec<ThemeSpec>,
    pub active: Option<String>,
}

/// The themes library resource.
pub struct Themes;

impl Resource for Themes {
    const KIND: &'static str = "themes.library";
    const LABEL: &'static str = "Themes";
    const ACTION_TYPE: &'static str = THEMES_ACTION_TYPE;

    type State = ThemesState;
    type Action = ThemesAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<ThemesState, ModelError> {
        Ok(ThemesState {
            catalog: doc::load_catalog(doc, &ROOT).unwrap_or_default(),
            active: ui::doc::read_str(doc, &ROOT, KEY_ACTIVE),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        // `new` / apply / export stay Effect or prompted ActionSpecs on the
        // published tree. Stub declarations here would shadow those.
        Vec::new()
    }

    fn search(state: &ThemesState) -> Vec<SearchEntry> {
        state
            .catalog
            .iter()
            .map(|theme| SearchEntry {
                anchor: Some(format!("themes.theme:{}", theme.id)),
                title: theme.name.clone(),
                text: theme.id.clone(),
            })
            .collect()
    }
}

/// Framework app: the themes widget is the surface over [`Themes`].
pub struct ThemesShell;

impl ResourceApp for ThemesShell {
    type Resource = Themes;
    const ID: &'static str = "so.tilo.themes";
    const TITLE: &'static str = "Themes";
    const ICON: Option<&'static str> = Some("themes");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Themes>) -> Element {
        cx.surface(ThemesApp::new())
    }
}

impl ResourceSurface<Themes> for ThemesApp {
    fn present(&mut self, state: &std::rc::Rc<ThemesState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Themes>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Themes>>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Themes>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &ThemesAction,
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

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }
}

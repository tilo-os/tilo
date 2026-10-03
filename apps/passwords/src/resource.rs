//! Passwords as a [`tilo_resource::Resource`]: shared filter/search/selection
//! prefs sync in the document; the encrypted vault catalogue stays local.
//!
//! Add / import / export are Effects with no document delta (published via
//! [`Resource::local_operations`]). Per-item copy / reveal / edit / delete
//! ride the live item extras — never a second hand-built vault root beside
//! [`tilo_resource::publish`].

use serde::{Deserialize, Serialize};
use ui::automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{
    Action, ActionError, ActionParam, ActionRegistry, ActionSpec, EncodedAction, OperationKind,
    ParamType, Resource as PublishedResource,
};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{self, VAULT_RESOURCE_ID};
use crate::doc::ViewDoc;
use crate::showcase::{PasswordsSurface, VaultFilter};

/// Collaborative vault filter, search, and selection preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PasswordsAction {
    SetFilter { filter: String },
    SetSearch { value: String },
    SelectItem { id: String },
}

impl Action for PasswordsAction {
    fn apply(&self, doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        match self {
            Self::SetFilter { filter } => ViewDoc::set_filter(doc, filter),
            Self::SetSearch { value } => ViewDoc::set_search(doc, value),
            Self::SelectItem { id } => ViewDoc::set_selected_id(doc, Some(id.as_str())),
        }
        Ok(())
    }
}

/// View projection stored in the shared document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VaultState {
    pub(crate) filter: VaultFilter,
    pub search: String,
    pub selected_id: Option<String>,
}

/// The password vault resource.
pub struct Passwords;

impl Resource for Passwords {
    const KIND: &'static str = "passwords.vault";
    const LABEL: &'static str = "Password vault";
    const ACTION_TYPE: &'static str = "passwords.action";

    type State = VaultState;
    type Action = PasswordsAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<PasswordsAction>(Self::ACTION_TYPE);
    }

    fn read(doc: &Doc) -> Result<VaultState, ModelError> {
        Ok(VaultState {
            filter: VaultFilter::parse(&ViewDoc::filter(doc)),
            search: ViewDoc::search(doc),
            selected_id: ViewDoc::selected_id(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "set-filter",
                "Filter vault",
                vec![ActionParam::required(
                    "filter",
                    ParamType::Text,
                    "all | favorites | login | api-key | note | wifi | other",
                )],
                |args, _| {
                    let filter = args
                        .text("filter")
                        .ok_or_else(|| "missing filter".to_owned())?
                        .to_owned();
                    Ok(PasswordsAction::SetFilter { filter })
                },
            )
            .describe("Set the shared vault sidebar filter."),
            ActionDecl::new(
                "set-search",
                "Search vault",
                vec![ActionParam::optional(
                    "query",
                    ParamType::Text,
                    "Text to find in titles, usernames, origins, or tags",
                )],
                |args, _| {
                    Ok(PasswordsAction::SetSearch {
                        value: args.text("query").unwrap_or("").to_owned(),
                    })
                },
            )
            .describe("Set the shared vault search string."),
            ActionDecl::new(
                "select-item",
                "Select item",
                vec![ActionParam::required(
                    "id",
                    ParamType::Text,
                    "Opaque local secret id",
                )],
                |args, _| {
                    let id = args
                        .text("id")
                        .ok_or_else(|| "missing id".to_owned())?
                        .to_owned();
                    Ok(PasswordsAction::SelectItem { id })
                },
            )
            .describe("Focus an opaque vault item in the shared selection."),
        ]
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Effects / Views with no document delta. Per-item ops ride live extras
        // (or the standalone overlay). Form save/cancel/generate are overlaid
        // when the editor is open.
        actions::vault_effect_specs()
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }
}

/// Hosted-only extras that are not document state: live vault item rows (and
/// editor-session ops when drafting). The vault door stays on
/// [`tilo_resource::publish`].
pub(crate) fn live_extras(app: &PasswordsSurface) -> Vec<PublishedResource> {
    app.live_item_extras()
}

/// Framework app: the passwords surface over [`Passwords`].
pub struct PasswordsShell;

impl ResourceApp for PasswordsShell {
    type Resource = Passwords;
    const ID: &'static str = "so.tilo.passwords";
    const TITLE: &'static str = "Passwords";
    const ICON: Option<&'static str> = Some("passwords");
    const SHOT_VIEWPORT: (u32, u32) = (1100, 720);
    // The window shows secrets: captures, recordings and remote viewers get
    // a placeholder in every launch, not only in a standalone build.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        capture_protected: true,
        ..ui::application::ComponentBehaviour::DEFAULT
    };

    fn view(cx: &mut ViewCx<Passwords>) -> Element {
        cx.surface(PasswordsSurface::live())
    }
}

impl ResourceSurface<Passwords> for PasswordsSurface {
    fn present(&mut self, state: &std::rc::Rc<VaultState>) {
        // Adopt collaborative view prefs only — never clear the local vault cache.
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Passwords>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Passwords>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    pub(crate) hosted: bool,
    pub(crate) last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Passwords>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    pub(crate) fn dispatch(&mut self, doc: &mut AutoCommit, action: &PasswordsAction) {
        let context =
            ui::tilo_actions::ActionContext::for_action("so.tilo.passwords", "Passwords", action);
        match &self.framework {
            Some(dispatcher) => {
                self.last = EncodedAction::encode(Passwords::ACTION_TYPE, action).ok();
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, Passwords::ACTION_TYPE, action, context),
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

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

/// Standalone / `--shots` / unit-test tree via [`tilo_resource::publish`].
pub fn provide_resources(state: &VaultState) -> Vec<PublishedResource> {
    vec![ui::tilo_resource::publish::<Passwords>(
        VAULT_RESOURCE_ID,
        "Password vault",
        state,
    )]
}

/// Live launch with capture protection for stored secrets.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<PasswordsShell>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::Widget;

    #[test]
    fn every_launch_withholds_the_vault_window_from_captures() {
        // The framework applies the declaration to every window hosting the
        // component; mounting the live view here would open the user's vault.
        const { assert!(<PasswordsShell as ResourceApp>::BEHAVIOUR.capture_protected) };
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario / publish-ownership assertion kept top-to-bottom"
    )]
    fn resource_publish_owns_vault_door_and_hosted_keeps_only_live_extras() {
        let mut doc = AutoCommit::new();
        PasswordsAction::SetFilter {
            filter: "favorites".into(),
        }
        .apply(&mut doc)
        .unwrap();
        PasswordsAction::SetSearch {
            value: "bank".into(),
        }
        .apply(&mut doc)
        .unwrap();
        PasswordsAction::SelectItem {
            id: "opaque-demo".into(),
        }
        .apply(&mut doc)
        .unwrap();
        doc.commit();

        let state = Passwords::read(&doc).expect("seeded vault state");
        let published =
            ui::tilo_resource::publish::<Passwords>(VAULT_RESOURCE_ID, "Password vault", &state);
        assert_eq!(published.kind, "passwords.vault");
        for id in [
            "set-filter",
            "set-search",
            "select-item",
            "add",
            "import",
            "export",
        ] {
            assert!(
                published.actions.iter().any(|action| action.id == id),
                "{id} must be on the framework resource"
            );
        }
        assert!(
            published.children.is_empty(),
            "shared projection must not carry live vault items"
        );
        assert_eq!(state.filter, VaultFilter::Favorites);
        assert_eq!(state.search, "bank");
        assert_eq!(state.selected_id.as_deref(), Some("opaque-demo"));

        let mut app = PasswordsSurface::demo(crate::ShowcaseMode::Vault);
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "passwords.vault");
        assert!(
            standalone[0]
                .children
                .iter()
                .any(|child| child.kind.starts_with("passwords.")),
            "standalone publishes vault item children from the local cache"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        assert!(ui::resources::resource(&app, &seeded, VAULT_RESOURCE_ID).is_none());
        let item_id = standalone[0]
            .children
            .iter()
            .find(|child| child.kind.starts_with("passwords."))
            .map(|child| child.id.as_str().to_owned())
            .expect("standalone item");
        assert!(
            ui::resources::resource(&app, &seeded, &item_id).is_some(),
            "a hosted vault item stays the published row"
        );
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "passwords.vault"),
            "hosted must not republish passwords.vault beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.kind == "passwords.login"
                    || resource.kind == "passwords.api-key"
                    || resource.kind == "passwords.note"
                    || resource.kind == "passwords.wifi"
                    || resource.kind == "passwords.secret"),
            "hosted still publishes live vault item rows: {:?}",
            extras.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>()
        );

        let door = provide_resources(&state);
        assert_eq!(door[0].kind, "passwords.vault");
        assert!(door[0].children.is_empty());

        let mut local = AutoCommit::new();
        let mut standalone = PasswordsSurface::demo(crate::ShowcaseMode::Vault);
        standalone.commit_view(
            &mut local,
            &PasswordsAction::SetSearch {
                value: "privacy".into(),
            },
        );
        assert!(
            standalone.last_document_action().is_some(),
            "ActionSink must record document actions on the local host"
        );
        assert_eq!(ViewDoc::search(&local), "privacy");

        // Empty present must not wipe a local vault that has not been published.
        let before = standalone.item_count();
        assert!(before > 0, "demo vault starts with items");
        standalone.adopt_resource_state(&VaultState::default());
        assert_eq!(
            standalone.item_count(),
            before,
            "adopting empty view prefs must not clear the vault cache"
        );
    }
}

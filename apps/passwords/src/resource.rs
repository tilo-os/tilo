//! Passwords as a [`tilo_resource::Resource`]: device-local secrets, View/Effect
//! door via [`Resource::local_operations`]. There is no collaborative document
//! vocabulary — the encrypted vault is not an Automerge record.

use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::vault_resource;
use crate::showcase::PasswordsSurface;

/// No collaborative document mutations: saves go to the encrypted local store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmptyDocAction {}

impl Action for EmptyDocAction {
    fn apply(&self, _doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        Ok(())
    }
}

/// Opaque vault projection. Secrets never enter the shared document; `read`
/// therefore returns an empty catalogue (ids are resolved by Effects locally).
#[derive(Clone, Debug, Default)]
pub struct VaultState;

/// The password vault resource.
pub struct Passwords;

impl Resource for Passwords {
    const KIND: &'static str = "passwords.vault";
    const LABEL: &'static str = "Password vault";
    const ACTION_TYPE: &'static str = "passwords.action";

    type State = VaultState;
    type Action = EmptyDocAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<EmptyDocAction>(Self::ACTION_TYPE);
    }

    fn read(_doc: &Doc) -> Result<VaultState, ModelError> {
        Ok(VaultState)
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Vault-level View/Effect ops only. Per-item copy/reveal/delete stay on
        // children built when the surface knows which items exist locally.
        vault_resource(false)
            .actions
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }
}

/// Framework app: the passwords surface over [`Passwords`].
pub struct PasswordsShell;

impl ResourceApp for PasswordsShell {
    type Resource = Passwords;
    const ID: &'static str = "so.tilo.passwords";
    const TITLE: &'static str = "Passwords";
    const ICON: Option<&'static str> = Some("passwords");
    const SHOT_VIEWPORT: (u32, u32) = (1100, 720);

    fn view(cx: &mut ViewCx<Passwords>) -> Element {
        cx.surface(PasswordsSurface::live())
    }
}

impl ResourceSurface<Passwords> for PasswordsSurface {
    fn present(&mut self, _state: &std::rc::Rc<VaultState>) {}

    fn attach(&mut self, _cx: SurfaceCx<Passwords>) {}
}

/// Live launch: framework host, capture-protected (secrets on screen).
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<PasswordsShell>().configure_app(|app| {
        app.set_window_title(Some("Passwords".into()));
        app.set_capture_protected(true);
    })
}

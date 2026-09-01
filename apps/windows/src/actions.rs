//! Palette/automation contract for Windows compatibility decisions.
//!
//! Launch, VM resume, permission changes, repair, and snapshot operations are
//! external service calls and therefore are deliberately not CRDT actions. The
//! selected route is the one durable, user-authored policy value, so it is a
//! replayable action. This keeps collaboration from becoming remote process
//! control while allowing a team to agree on a launch policy.
use crate::model::{Catalogue, Route};
use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
use qos_actions::{
    Action, ActionError, ActionInput, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, Resource,
};
use serde::{Deserialize, Serialize};

pub const WINDOWS_ACTION_TYPE: &str = "windows.compat.action";
const ROOT_KEY: &str = "windows-compat";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowsAction {
    SetRoute { app_id: String, route: Route },
    ResetRoute { app_id: String },
}

impl Action for WindowsAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let map = match doc.get(&ROOT, ROOT_KEY)? {
            Some((automerge::Value::Object(ObjType::Map), id)) => id,
            _ => doc.put_object(&ROOT, ROOT_KEY, ObjType::Map)?,
        };
        match self {
            Self::SetRoute { app_id, route } => doc.put(&map, app_id, route.label())?,
            Self::ResetRoute { app_id } => {
                doc.delete(&map, app_id)?;
            }
        }
        Ok(())
    }
    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

pub fn register(registry: &mut ActionRegistry) {
    registry.register::<WindowsAction>(WINDOWS_ACTION_TYPE);
}
pub fn encode(action: &WindowsAction) -> EncodedAction {
    EncodedAction::encode(WINDOWS_ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: WINDOWS_ACTION_TYPE.into(),
        payload: Vec::new(),
    })
}
fn immediate(id: &str, label: &str, action: &WindowsAction) -> ActionSpec {
    ActionSpec::immediate(id, label, WINDOWS_ACTION_TYPE, action).unwrap_or_else(|_| ActionSpec {
        id: id.into(),
        label: label.into(),
        description: None,
        locus: ExecutionLocus::ClientLocal,
        input: ActionInput::Immediate(encode(action)),
    })
}

/// The resource tree intentionally exposes policy only. Operations that can
/// run guest code are not exported remotely; they go through the local service
/// with a portal-mediated user confirmation.
pub fn provide_resources(catalogue: &Catalogue) -> Vec<Resource> {
    catalogue
        .apps
        .iter()
        .map(|app| {
            let mut resource = Resource::new(
                format!("windows.app:{}", app.id),
                "windows.compat-app",
                &app.name,
            );
            for route in Route::ALL {
                resource = resource.with_action(immediate(
                    &format!("route-{}", route.short().to_ascii_lowercase()),
                    &format!("Use {}", route.label()),
                    &WindowsAction::SetRoute {
                        app_id: app.id.clone(),
                        route,
                    },
                ));
            }
            if app.uses_override() {
                resource = resource.with_action(immediate(
                    "reset-route",
                    "Use catalogue recommendation",
                    &WindowsAction::ResetRoute {
                        app_id: app.id.clone(),
                    },
                ));
            }
            resource
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::CompatibilityProvider;
    #[test]
    fn route_policy_is_deterministic_and_reset_removes_it() {
        let mut doc = Doc::new();
        WindowsAction::SetRoute {
            app_id: "reader".into(),
            route: Route::Proton,
        }
        .apply(&mut doc)
        .unwrap();
        let (_, map) = doc.get(&ROOT, ROOT_KEY).unwrap().unwrap();
        assert_eq!(
            doc.get(&map, "reader").unwrap().unwrap().0.to_str(),
            Some("Proton")
        );
        WindowsAction::ResetRoute {
            app_id: "reader".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert!(doc.get(&map, "reader").unwrap().is_none());
    }
    #[test]
    fn every_catalogue_app_has_explicit_routes() {
        let res = provide_resources(&crate::provider::SampleProvider::new().catalogue());
        assert!(res.iter().all(|r| r.actions.len() >= 3));
    }
}

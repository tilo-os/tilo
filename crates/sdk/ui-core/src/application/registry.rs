//! The applications a multi-app host can mount, registered once per process.
//!
//! A host that links applications in-process (the browser's desktop bundle,
//! a shell hosting apps in-process) reads this list instead of naming apps in
//! its own code. Registration is idempotent per app id: the first definition
//! for an id wins, so a bundle that lists an app twice still shows it once.

use std::sync::Mutex;

use super::ApplicationDefinition;

static REGISTRY: Mutex<Vec<&'static ApplicationDefinition>> = Mutex::new(Vec::new());

/// Register `definition`. Returns `false` when an application with the same
/// id is already registered; that earlier definition stays.
pub fn register(definition: &'static ApplicationDefinition) -> bool {
    let mut registry = REGISTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if registry
        .iter()
        .any(|registered| registered.app_id == definition.app_id)
    {
        return false;
    }
    registry.push(definition);
    true
}

/// Every registered application, in registration order.
pub fn applications() -> Vec<&'static ApplicationDefinition> {
    REGISTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The registered application with id `app_id`.
pub fn application(app_id: &str) -> Option<&'static ApplicationDefinition> {
    REGISTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .copied()
        .find(|definition| definition.app_id == app_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::RecordKind;
    use crate::widget::IconDescriptor;

    fn definition(app_id: &'static str) -> &'static ApplicationDefinition {
        Box::leak(Box::new(ApplicationDefinition {
            app_id,
            title: "Probe",
            icon: IconDescriptor::Named("notes".into()),
            record_kind: RecordKind::new("probe.page"),
            required: vec![],
            optional: vec![],
            opens: &[],
            create: |_, _, _| Err(crate::launch::LaunchError::MissingComponentHost),
        }))
    }

    #[test]
    fn first_registration_per_id_wins() {
        let first = definition("test.registry.first-wins");
        let second = definition("test.registry.first-wins");
        assert!(register(first));
        assert!(!register(second));
        let found = application("test.registry.first-wins").unwrap();
        assert!(std::ptr::eq(found, first));
        assert_eq!(
            applications()
                .iter()
                .filter(|d| d.app_id == "test.registry.first-wins")
                .count(),
            1
        );
    }
}

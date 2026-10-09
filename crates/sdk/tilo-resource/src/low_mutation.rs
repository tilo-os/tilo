//! Bounded, default-deny typed state mutations without argument-risk admission.
use tilo_actions::{ActionArgs, ActionSpec, EncodedAction, ExecutionLocus, PermissionRequirement};

use crate::Resource;

/// A typed mutation explicitly audited by its resource and classified Low.
pub const APPLY_LOW: &str = "apply-low";
/// An atomic group containing only audited Low mutations.
pub const GROUP_LOW: &str = "group-low";
const MAX_BYTES: usize = 1_048_576;
const MAX_MEMBERS: usize = 256;

fn bounded(arguments: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let bytes = serde_json::to_vec(arguments).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("low mutation exceeds 1048576 encoded bytes".into());
    }
    Ok(())
}

fn check<R: Resource>(action: &R::Action) -> Result<(), String> {
    if !R::AUDITED_LOW_MUTATIONS
        || !R::allows_low_mutation(action)
        || tilo_actions::Action::risk(action) != tilo_actions::RiskLevel::Low
    {
        return Err("mutation is not an audited Low action".into());
    }
    Ok(())
}

pub(crate) fn build_action<R: Resource>(
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<R::Action, String> {
    bounded(arguments)?;
    let action = crate::resource::build_action::<R>(
        crate::APPLY,
        arguments,
        crate::resource::origin_now_ms(),
    )?;
    check::<R>(&action)?;
    Ok(action)
}

fn build_group<R: Resource>(
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<crate::ActionGroup<R::Action>, String> {
    bounded(arguments)?;
    let members = arguments
        .get(crate::GROUP_PARAM)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "actions must be an array".to_owned())?;
    if members.is_empty() || members.len() > MAX_MEMBERS {
        return Err("low mutation group requires 1..256 members".into());
    }
    let group = crate::group::build_group::<R>(arguments, crate::resource::origin_now_ms())?;
    for action in &group.actions {
        check::<R>(action)?;
    }
    Ok(group)
}

pub(crate) fn apply_spec<R: Resource>() -> ActionSpec {
    ActionSpec::typed(
        APPLY_LOW,
        format!("Apply audited {} edit", R::LABEL),
        "Audited state mutation",
        ExecutionLocus::ClientLocal,
        vec![crate::resource::apply_param::<R>()],
        |args| crate::resource::encode_built::<R>(build_action::<R>(&args.to_json())),
    )
    .programmatic()
    .reversible()
    .carrying()
    .requiring(R::least_permission())
    .with_permission_of(|args| {
        build_action::<R>(&args.to_json()).map_or(PermissionRequirement::Write, |action| {
            R::permission(&action)
        })
    })
}

pub(crate) fn group_spec<R: Resource>() -> ActionSpec {
    ActionSpec::typed(
        GROUP_LOW,
        format!("Apply audited {} edits as one", R::LABEL),
        "Audited state mutations",
        ExecutionLocus::ClientLocal,
        vec![crate::resource::group_param::<R>()],
        |args| match build_group::<R>(&args.to_json()) {
            Ok(group) => EncodedAction::encode(crate::group::group_type::<R>(), &group)
                .unwrap_or_else(|error| crate::resource::refused::<R>(&error.to_string())),
            Err(reason) => crate::resource::refused::<R>(&reason),
        },
    )
    .programmatic()
    .carrying()
    .reversible()
    .requiring(R::least_permission())
    .with_permission_of(group_permission::<R>)
}

fn group_permission<R: Resource>(args: &ActionArgs) -> PermissionRequirement {
    build_group::<R>(&args.to_json()).map_or(PermissionRequirement::Write, |group| {
        group
            .actions
            .iter()
            .map(R::permission)
            .max_by_key(|permission| crate::resource::permission_rank(*permission))
            .unwrap_or(PermissionRequirement::Write)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use tilo_actions::{Action, ActionError, RiskLevel};

    #[derive(Clone, Debug, Serialize, Deserialize)]
    enum Mutation {
        Safe(String),
        Risky {},
        Unaudited {},
    }

    impl Action for Mutation {
        fn apply(&self, _doc: &mut crate::Doc) -> Result<(), ActionError> {
            Ok(())
        }
        fn risk(&self) -> RiskLevel {
            match self {
                Self::Risky {} => RiskLevel::Destructive,
                _ => RiskLevel::Low,
            }
        }
    }

    struct Audited;
    impl Resource for Audited {
        const KIND: &'static str = "test.audited";
        const LABEL: &'static str = "Audited";
        const AUDITED_LOW_MUTATIONS: bool = true;
        type State = ();
        type Action = Mutation;
        fn read(_doc: &crate::Doc) -> Result<(), crate::ModelError> {
            Ok(())
        }
        fn allows_low_mutation(action: &Mutation) -> bool {
            matches!(action, Mutation::Safe(_) | Mutation::Risky {})
        }
    }
    struct DefaultDenied;
    impl Resource for DefaultDenied {
        const KIND: &'static str = "test.default-denied";
        const LABEL: &'static str = "Default denied";
        type State = ();
        type Action = Mutation;
        fn read(_doc: &crate::Doc) -> Result<(), crate::ModelError> {
            Ok(())
        }
    }

    fn door<R: Resource<Action = Mutation>>(
        value: Mutation,
    ) -> Result<EncodedAction, tilo_actions::DoorError> {
        let resource =
            crate::publish::<R>("record", "record", &R::read(&crate::Doc::new()).unwrap());
        let args = serde_json::json!({"action":serde_json::to_value(value).unwrap()})
            .as_object()
            .unwrap()
            .clone();
        tilo_actions::door::encode_json(
            &[resource],
            &tilo_actions::ActionAddress::new("record", APPLY_LOW),
            &args,
        )
    }

    #[test]
    fn public_low_door_checks_closed_opt_in_actual_risk_and_encoded_size() {
        assert!(door::<Audited>(Mutation::Safe("safe".into())).is_ok());
        assert!(matches!(door::<Audited>(Mutation::Risky {}),
            Err(tilo_actions::DoorError::InvalidArguments(reason)) if reason.contains("audited Low")));
        assert!(matches!(door::<Audited>(Mutation::Unaudited {}),
            Err(tilo_actions::DoorError::InvalidArguments(reason)) if reason.contains("audited Low")));
        assert!(door::<DefaultDenied>(Mutation::Safe("safe".into())).is_err());
        assert!(door::<Audited>(Mutation::Safe("x".repeat(MAX_BYTES))).is_err());
    }

    #[test]
    fn public_low_group_revalidates_named_members_and_rejects_count_overflow() {
        let resource = crate::publish::<Audited>("record", "record", &());
        let safe = serde_json::json!({"action":crate::APPLY,"args":{"action":Mutation::Safe("safe".into())}});
        let invoke = |members: Vec<serde_json::Value>| {
            let args = serde_json::json!({"actions":members})
                .as_object()
                .unwrap()
                .clone();
            tilo_actions::door::encode_json(
                std::slice::from_ref(&resource),
                &tilo_actions::ActionAddress::new("record", GROUP_LOW),
                &args,
            )
        };
        assert!(invoke(vec![safe.clone()]).is_ok());
        assert!(matches!(invoke(vec![safe.clone(); MAX_MEMBERS + 1]),
            Err(tilo_actions::DoorError::InvalidArguments(reason)) if reason.contains("1..256")));
        let risky = serde_json::json!({"action":crate::APPLY,"args":{"action":Mutation::Risky {}}});
        assert!(matches!(invoke(vec![safe, risky]),
            Err(tilo_actions::DoorError::InvalidArguments(reason)) if reason.contains("audited Low")));
    }

    #[test]
    fn default_resource_keeps_only_its_original_typed_mutation_catalogue() {
        let resource = crate::publish::<DefaultDenied>("record", "record", &());
        assert_eq!(
            resource
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>(),
            vec![crate::APPLY, crate::GROUP]
        );
        let grouped = resource
            .actions
            .iter()
            .find(|action| action.id == crate::GROUP)
            .unwrap();
        assert!(
            !serde_json::to_string(&grouped.params())
                .unwrap()
                .contains(APPLY_LOW)
        );
    }

    #[test]
    fn typed_dispatch_selection_preserves_untrusted_and_risky_original_door() {
        assert_eq!(
            crate::Invoke::<Audited>::checked_apply(&Mutation::Safe("s".into()))
                .unwrap()
                .id,
            APPLY_LOW
        );
        for action in [Mutation::Risky {}, Mutation::Unaudited {}] {
            assert_eq!(
                crate::Invoke::<Audited>::checked_apply(&action).unwrap().id,
                crate::APPLY
            );
        }
        assert_eq!(
            crate::Invoke::<DefaultDenied>::checked_apply(&Mutation::Safe("s".into()))
                .unwrap()
                .id,
            crate::APPLY
        );
    }
}

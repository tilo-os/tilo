//! `identity.device.enroll` and `identity.home-peer.enroll`: mint a setup
//! code another device redeems to join this person.
//!
//! The code is copied for the person who picked the row, and returned in the
//! action's result for a script or agent to hand to the joining device. It is
//! a bearer credential, so the actions are `Sensitive` and need an explicit
//! confirmation; a result is never kept in shared history (effect receipts
//! redact it).

use tilo_identity_host::enrollment::EnrollmentRole;

use crate::shell::chrome_state::ChromeState;
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};

pub(super) const DEVICE_ENROLL_ID: &str = "identity.device.enroll";
pub(super) const HOME_PEER_ENROLL_ID: &str = "identity.home-peer.enroll";

/// How long a code lives unless the caller asks for less or more.
const DEFAULT_TTL_SECONDS: i64 = 5 * 60;
/// The bounds a caller may choose a code's life within: long enough to type
/// it on another device, short enough that a leaked code soon stops working.
const MIN_TTL_SECONDS: i64 = 30;
const MAX_TTL_SECONDS: i64 = 15 * 60;

pub(super) const ENROLL_PARAMS: &[ActionParam] = &[
    ActionParam::optional(
        "ttl_seconds",
        ParamType::Integer,
        "How long the code stays valid, 30 to 900 seconds (defaults to 300).",
    ),
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Explicit confirmation that a code admitting another device may be issued.",
    ),
];

/// The role an enroll action's code admits a device as.
pub(super) fn role_of(id: &str) -> Option<EnrollmentRole> {
    match id {
        DEVICE_ENROLL_ID => Some(EnrollmentRole::PersonalDevice),
        HOME_PEER_ENROLL_ID => Some(EnrollmentRole::HomePeer),
        _ => None,
    }
}

/// The contract both enroll actions share: they reach outside the document,
/// hand out a credential, and say what they return.
pub(super) fn contract() -> tilo_actions::ActionContract {
    tilo_actions::ActionContract {
        kind: tilo_actions::ActionKind::Outside,
        risk: tilo_actions::RiskLevel::Sensitive,
        confirmation: tilo_actions::ConfirmationPolicy::Confirm,
        retry: tilo_actions::RetryPolicy::AtMostOnce,
        result: tilo_actions::ResultSpec::Json {
            schema: serde_json::json!({
                "type": "object",
                "required": ["token", "role", "expires_at_ns", "copied"],
                "properties": {
                    "token": { "type": "string" },
                    "role": { "type": "string", "enum": ["personal-device", "home-peer"] },
                    "expires_at_ns": { "type": "integer" },
                    "copied": { "type": "boolean" }
                }
            }),
        },
        ..tilo_actions::ActionContract::default()
    }
}

fn ttl_ns(args: &ActionArgs) -> Result<u64, ActionError> {
    let seconds = args.integer("ttl_seconds").unwrap_or(DEFAULT_TTL_SECONDS);
    if !(MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&seconds) {
        return Err(ActionError::Refused(format!(
            "ttl_seconds must be between {MIN_TTL_SECONDS} and {MAX_TTL_SECONDS}"
        )));
    }
    u64::try_from(seconds)
        .map(|seconds| seconds.saturating_mul(1_000_000_000))
        .map_err(|_| ActionError::Refused("ttl_seconds must be positive".into()))
}

/// Mint a code for `role` from this device, copy it, and answer it.
///
/// Who may run it is checked first: only someone who administers the active
/// workspace's identity may admit a device to it.
pub(super) fn mint(
    state: &ChromeState,
    args: &ActionArgs,
    role: EnrollmentRole,
) -> Result<serde_json::Value, ActionError> {
    let (_, profile) = super::active_identity(state)?;
    super::require_explicit_confirmation(args, "issue a device setup code")?;
    let ttl = ttl_ns(args)?;
    let now = tilo_clock::unix_now_ns();
    let expires_at_ns = now.saturating_add(ttl);
    let token = crate::device_fleet::DeviceFleetStore::from_env()
        .and_then(|store| store.mint_enrollment(&profile.id, role.preset(), now, expires_at_ns))
        .map_err(|error| ActionError::Refused(error.to_string()))?;
    // The row's subtitle states the copy and the code's life. The clipboard
    // holds the code no longer than the code lasts, and keeps it out of
    // history and device sync: it admits a device.
    let copied = ui::try_clipboard_copy_secret(&token, std::time::Duration::from_nanos(ttl));
    Ok(serde_json::json!({
        "token": token,
        "role": role.as_str(),
        "expires_at_ns": expires_at_ns,
        "copied": copied,
    }))
}

/// [`mint`] for the person who picked the row: the clipboard is out of sight
/// and search closes on choose, so the shell says in passing that the code is
/// copied and how long it lasts.
pub(super) fn mint_for_person(
    state: &mut ChromeState,
    args: &ActionArgs,
    role: EnrollmentRole,
) -> Result<(), ActionError> {
    let minutes = ttl_ns(args)?.div_ceil(60 * 1_000_000_000);
    let minted = mint(state, args, role)?;
    let copied = minted
        .get("copied")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let key = match (role, copied) {
        (_, false) => "search-action-enrollment-copy-failed",
        (EnrollmentRole::PersonalDevice, true) => "search-action-enrollment-copied",
        (EnrollmentRole::HomePeer, true) => "search-action-home-server-copied",
    };
    state.acknowledge(crate::shell::i18n::t_args(
        key,
        &[("minutes", minutes.into())],
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::search::action_api::ArgValue;

    fn args(ttl: Option<i64>, confirm: bool) -> ActionArgs {
        let mut values = vec![("confirm".to_string(), ArgValue::Boolean(confirm))];
        if let Some(ttl) = ttl {
            values.push(("ttl_seconds".to_string(), ArgValue::Integer(ttl)));
        }
        ActionArgs::bind(ENROLL_PARAMS, values).expect("enroll arguments bind")
    }

    #[test]
    fn enroll_codes_have_a_bounded_life_and_one_role_each() {
        assert_eq!(
            ttl_ns(&args(None, true)).expect("default"),
            300 * 1_000_000_000
        );
        assert_eq!(
            ttl_ns(&args(Some(900), true)).expect("longest"),
            900 * 1_000_000_000
        );
        for refused in [0, 29, 901, -5] {
            assert!(
                ttl_ns(&args(Some(refused), true)).is_err(),
                "{refused} seconds is out of bounds"
            );
        }
        assert_eq!(
            role_of(DEVICE_ENROLL_ID),
            Some(EnrollmentRole::PersonalDevice)
        );
        assert_eq!(role_of(HOME_PEER_ENROLL_ID), Some(EnrollmentRole::HomePeer));
        assert_eq!(role_of("identity.device.revoke"), None);

        let contract = contract();
        assert!(contract.risk.requires_confirmation());
        assert_eq!(
            contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        let answer = tilo_actions::ActionOutput {
            value: Some(serde_json::json!({
                "token": "ABCD",
                "role": EnrollmentRole::HomePeer.as_str(),
                "expires_at_ns": 1,
                "copied": true,
            })),
            resources: Vec::new(),
        };
        contract
            .result
            .validate(&answer)
            .expect("the answer matches the advertised result");
    }
}

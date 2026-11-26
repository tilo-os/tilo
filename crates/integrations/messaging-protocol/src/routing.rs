//! Final external-route checks against Tilo's canonical account registry.
//! This never selects an active/default account or resolves contacts by name.
//! The broker supplies current linked-device authority from its adapter, not
//! from editable account config. These checks complement its capability check.

use crate::journal::Owner;
use crate::{Route, Service};
use accounts::person::AccountList;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    Linked,
    Unavailable,
    Revoked,
}

pub struct LiveBinding {
    pub owner: Owner,
    pub state: LinkState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    AccountMissing,
    AccountDisabled,
    WrongService,
    WrongAccount,
    WrongDevice,
    WrongActor,
    StaleLink,
    Unavailable,
    Revoked,
}

/// Check the exact frozen route immediately before the journal/send boundary.
/// The current binding must come from the host's account-owned adapter and
/// certified device authority, never from user-supplied action parameters.
pub fn check(
    accounts: &AccountList,
    route: &Route,
    expected: &Owner,
    live: &LiveBinding,
) -> Result<(), Refusal> {
    if route.service() == Service::Tilo {
        return Err(Refusal::WrongService);
    }
    check_owned(accounts, route, expected, live)
}

pub(crate) fn check_native(
    accounts: &AccountList,
    route: &Route,
    expected: &Owner,
    live: &LiveBinding,
) -> Result<(), Refusal> {
    if route.service() != Service::Tilo {
        return Err(Refusal::WrongService);
    }
    check_owned(accounts, route, expected, live)
}
fn check_owned(
    accounts: &AccountList,
    route: &Route,
    expected: &Owner,
    live: &LiveBinding,
) -> Result<(), Refusal> {
    if !expected.valid_actor() || expected.actor != live.owner.actor {
        return Err(Refusal::WrongActor);
    }
    if route.account() != expected.account || route.account() != live.owner.account {
        return Err(Refusal::WrongAccount);
    }
    if route.service() != expected.service || route.service() != live.owner.service {
        return Err(Refusal::WrongService);
    }
    if expected.device != live.owner.device {
        return Err(Refusal::WrongDevice);
    }
    if expected.linking_generation != live.owner.linking_generation {
        return Err(Refusal::StaleLink);
    }
    let account = accounts
        .account(route.account())
        .ok_or(Refusal::AccountMissing)?;
    if !account.enabled {
        return Err(Refusal::AccountDisabled);
    }
    let required = match route.service() {
        Service::Whatsapp => "whatsapp",
        Service::Signal => "signal",
        Service::Sms => "sms",
        Service::Tilo => "tilo",
    };
    if account.type_id.as_str() != required {
        return Err(Refusal::WrongService);
    }
    match live.state {
        LinkState::Linked => Ok(()),
        LinkState::Unavailable => Err(Refusal::Unavailable),
        LinkState::Revoked => Err(Refusal::Revoked),
    }
}

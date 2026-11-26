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

#[cfg(test)]
mod tests {
    use super::*;
    use accounts::Account;
    fn owner() -> Owner {
        Owner {
            actor: "11".repeat(32),
            device: "device-a".into(),
            service: Service::Signal,
            account: "account-a".into(),
            linking_generation: 1,
        }
    }
    fn route() -> Route {
        Route::new(Service::Signal, "account-a".into(), "recipient".into()).unwrap()
    }
    fn store() -> AccountList {
        let mut store = AccountList::default();
        let mut account = Account::new("account-a", "signal");
        account.display_name = "Same display name".into();
        account.enabled = true;
        store.keep(&account).unwrap();
        let mut another = Account::new("account-b", "signal");
        another.display_name = "Same display name".into();
        another.enabled = true;
        store.keep(&another).unwrap();
        store
    }

    tilo_test_env::scenario! {
        /// `check`: which account, link and owner a submission is refused for.
        fn route_check;

        phase removed_selected_account_never_falls_back_to_another_with_the_same_name;
        {
            let live = LiveBinding {
                owner: owner(),
                state: LinkState::Linked,
            };
            assert_eq!(check(&store(), &route(), &owner(), &live), Ok(()));
            let mut store = AccountList::default();
            let mut other = Account::new("account-b", "signal");
            other.enabled = true;
            store.keep(&other).unwrap();
            assert_eq!(
                check(&store, &route(), &owner(), &live),
                Err(Refusal::AccountMissing)
            );
        }

        phase revoked_disabled_stale_or_wrong_device_authority_refuses_submission;
        {
            let mut live = LiveBinding {
                owner: owner(),
                state: LinkState::Revoked,
            };
            assert_eq!(
                check(&store(), &route(), &owner(), &live),
                Err(Refusal::Revoked)
            );
            live.state = LinkState::Unavailable;
            assert_eq!(
                check(&store(), &route(), &owner(), &live),
                Err(Refusal::Unavailable)
            );
            live.state = LinkState::Linked;
            live.owner.linking_generation = 2;
            assert_eq!(
                check(&store(), &route(), &owner(), &live),
                Err(Refusal::StaleLink)
            );
            live.owner = owner();
            live.owner.device = "device-b".into();
            assert_eq!(
                check(&store(), &route(), &owner(), &live),
                Err(Refusal::WrongDevice)
            );
            live.owner = owner();
            live.owner.actor = "22".repeat(32);
            assert_eq!(
                check(&store(), &route(), &owner(), &live),
                Err(Refusal::WrongActor)
            );
            live.owner = owner();
            let mut disabled = AccountList::default();
            let mut account = Account::new("account-a", "signal");
            account.enabled = false;
            disabled.keep(&account).unwrap();
            assert_eq!(
                check(&disabled, &route(), &owner(), &live),
                Err(Refusal::AccountDisabled)
            );
            let mut wrong = AccountList::default();
            let mut account = Account::new("account-a", "sms");
            account.enabled = true;
            wrong.keep(&account).unwrap();
            assert_eq!(
                check(&wrong, &route(), &owner(), &live),
                Err(Refusal::WrongService)
            );
        }
    }
}

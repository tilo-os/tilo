//! Final external-route checks against Tilo's canonical account registry.
//! This never selects an active/default account or resolves contacts by name.
//! The broker supplies current linked-device authority from its adapter, not
//! from editable account config. These checks complement its capability gate.

use crate::journal::Owner;
use crate::{Route, Service};
use accounts::{AccountId, AccountStore};

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
    AccountNotOwned,
    WrongService,
    WrongAccount,
    WrongDevice,
    WrongPrincipal,
    StaleLink,
    Unavailable,
    Revoked,
}

/// Check the exact frozen route immediately before the journal/send boundary.
/// The current binding must come from the host's account-owned adapter and
/// certified device authority, never from user-supplied action parameters.
///
/// # Errors
/// Refuses missing/disabled accounts, wrong service/device, revoked or stale
/// linking and unavailable transports. No alternate route is returned.
pub fn check(
    store: &AccountStore,
    route: &Route,
    expected: &Owner,
    live: &LiveBinding,
) -> Result<(), Refusal> {
    if route.service() == Service::Tilo {
        return Err(Refusal::WrongService);
    }
    check_owned(store, route, expected, live)
}

/// Check a direct private Tilo route without admitting it to external adapters.
pub(crate) fn check_native(
    store: &AccountStore,
    route: &Route,
    expected: &Owner,
    live: &LiveBinding,
) -> Result<(), Refusal> {
    if route.service() != Service::Tilo {
        return Err(Refusal::WrongService);
    }
    check_owned(store, route, expected, live)
}
fn check_owned(
    store: &AccountStore,
    route: &Route,
    expected: &Owner,
    live: &LiveBinding,
) -> Result<(), Refusal> {
    if !expected.valid_principal() || expected.principal != live.owner.principal {
        return Err(Refusal::WrongPrincipal);
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
    let account = store
        .get(&AccountId::new(route.account()))
        .ok_or(Refusal::AccountMissing)?;
    if !account.enabled {
        return Err(Refusal::AccountDisabled);
    }
    // Contributed workspace accounts need a separate credential delegation
    // grant; mere visibility in a store is not permission to act as its owner.
    if account.owner.is_some() {
        return Err(Refusal::AccountNotOwned);
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
            principal: "11".repeat(32),
            device: "device-a".into(),
            service: Service::Signal,
            account: "account-a".into(),
            linking_generation: 1,
        }
    }
    fn route() -> Route {
        Route::new(Service::Signal, "account-a".into(), "recipient".into()).unwrap()
    }
    fn store() -> AccountStore {
        let mut store = AccountStore::new();
        let mut account = Account::new("account-a", "signal");
        account.display_name = "Same display name".into();
        account.enabled = true;
        store.upsert(account);
        let mut another = Account::new("account-b", "signal");
        another.display_name = "Same display name".into();
        another.enabled = true;
        store.upsert(another);
        store
    }

    #[test]
    fn removed_selected_account_never_falls_back_to_another_with_the_same_name() {
        let live = LiveBinding {
            owner: owner(),
            state: LinkState::Linked,
        };
        assert_eq!(check(&store(), &route(), &owner(), &live), Ok(()));
        let mut store = AccountStore::new();
        let mut other = Account::new("account-b", "signal");
        other.enabled = true;
        store.upsert(other);
        assert_eq!(
            check(&store, &route(), &owner(), &live),
            Err(Refusal::AccountMissing)
        );
    }

    #[test]
    fn revoked_disabled_stale_or_wrong_device_authority_refuses_submission() {
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
        live.owner.principal = "22".repeat(32);
        assert_eq!(
            check(&store(), &route(), &owner(), &live),
            Err(Refusal::WrongPrincipal)
        );
        live.owner = owner();
        let mut disabled = AccountStore::new();
        let mut account = Account::new("account-a", "signal");
        account.enabled = false;
        disabled.upsert(account);
        assert_eq!(
            check(&disabled, &route(), &owner(), &live),
            Err(Refusal::AccountDisabled)
        );
        let mut wrong = AccountStore::new();
        let mut account = Account::new("account-a", "sms");
        account.enabled = true;
        wrong.upsert(account);
        assert_eq!(
            check(&wrong, &route(), &owner(), &live),
            Err(Refusal::WrongService)
        );
        let mut contributed = AccountStore::new();
        let mut account = Account::new("account-a", "signal");
        account.owner = Some("another-person".into());
        contributed.upsert(account);
        assert_eq!(
            check(&contributed, &route(), &owner(), &live),
            Err(Refusal::AccountNotOwned)
        );
    }
}

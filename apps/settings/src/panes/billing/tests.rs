//! What Billing says of an account and which of the provider's pages it opens.

use ui::tilo_actions::remote::host_account::HostKind;

use super::*;

fn en() {
    ui::i18n::I18nManager::init("en-US");
}

fn account(scenario: &str) -> HostAccount {
    crate::scenarios::billing_fixture(Some(scenario)).expect("a billing scenario")
}

/// An account on a plan says its plan, card, the Linux machine time it used against what the
/// plan includes, and its credit.
#[test]
fn an_account_on_a_plan_says_what_it_has_and_used() {
    en();
    assert_eq!(
        facts(&account("billing")),
        [
            ("Plan".to_owned(), "Pro, $20 a month".to_owned()),
            ("Card".to_owned(), "On file".to_owned()),
            (
                "Linux machines this month".to_owned(),
                "37 hr of 100 hr".to_owned()
            ),
            ("Credit".to_owned(), "$12.40".to_owned()),
        ]
    );
}

/// A new account has no card, and nothing asks for one before something needs a Linux machine.
#[test]
fn a_new_account_has_no_card_yet() {
    en();
    let new = account("billing-new");
    assert!(!has_card(&new));
    assert_eq!(
        facts(&new)[1..],
        [
            ("Card".to_owned(), "None yet".to_owned()),
            (
                "Linux machines this month".to_owned(),
                "None yet".to_owned()
            ),
        ]
    );
}

/// Manage billing and an invoice open the provider's pages the host answered, and nothing that
/// is not one of its web pages.
#[test]
fn the_providers_pages_open_and_nothing_else() {
    en();
    let mut cloud = account("billing");
    assert_eq!(
        page_of(&cloud, BillingEffect::ManageBilling).as_deref(),
        Ok("https://tilo.sh/billing")
    );
    assert_eq!(
        page_of(
            &cloud,
            BillingEffect::OpenInvoice {
                invoice: "in_1".into()
            }
        )
        .as_deref(),
        Ok("https://tilo.sh/billing/invoices/in_1")
    );
    assert!(
        page_of(
            &cloud,
            BillingEffect::OpenInvoice {
                invoice: "in_9".into()
            }
        )
        .is_err()
    );
    cloud.manage_billing = Some("file:///etc/passwd".into());
    assert!(page_of(&cloud, BillingEffect::ManageBilling).is_err());
}

/// A host that runs the workspaces itself sells nothing: no plan, no card, no page to open.
#[test]
fn a_self_hosted_host_sells_nothing() {
    en();
    let own = HostAccount::sells_nothing("Ada's server", HostKind::SelfHosted);
    assert!(!own.sells());
    assert!(page_of(&own, BillingEffect::ManageBilling).is_err());
}

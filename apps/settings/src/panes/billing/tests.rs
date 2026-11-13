//! What Billing says of an account and which of the provider's pages it opens.

use ui::tilo_actions::remote::host_account::HostKind;

use super::*;

fn en() {
    ui::i18n::I18nManager::init("en-US");
}

fn account(scenario: &str) -> HostAccount {
    crate::scenarios::billing_fixture(Some(scenario)).expect("a billing scenario")
}

#[test]
fn an_account_on_a_plan_says_what_it_has_and_used() {
    en();
    assert_eq!(
        facts(&account("billing")),
        [
            ("Plan".to_owned(), "Pro, $20 a month".to_owned()),
            ("Card".to_owned(), "Visa ·· 4242".to_owned()),
            (
                "Linux machines this month".to_owned(),
                "37 hr of 100 hr".to_owned()
            ),
            ("Credit".to_owned(), "$12.40".to_owned()),
        ]
    );
}

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

#[test]
fn a_self_hosted_host_sells_nothing() {
    en();
    let own = HostAccount::sells_nothing("Ada's server", HostKind::SelfHosted);
    assert!(!own.sells());
    assert!(page_of(&own, BillingEffect::ManageBilling).is_err());
}

#[test]
fn the_card_says_what_the_provider_describes() {
    en();
    let mut cloud = account("billing");
    let line = |cloud: &HostAccount| card_line(cloud);
    assert_eq!(line(&cloud), "Visa ·· 4242");
    let standing = cloud.standing.as_mut().unwrap();
    standing.payment_method = Some(PaymentMethod {
        brand: None,
        last4: Some("0005".into()),
    });
    assert_eq!(line(&cloud), "Card ·· 0005");
    cloud.standing.as_mut().unwrap().payment_method = Some(PaymentMethod::default());
    assert_eq!(line(&cloud), "On file");
}

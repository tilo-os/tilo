//! Direct messaging: shared Contacts identities, explicit account/recipient
//! choices and separate service histories. Transport state belongs to the
//! privileged broker; it is never inferred from names or widget state.

mod enrollment;
mod history;
#[cfg(test)]
mod history_tests;
mod sending;
mod setup;
#[cfg(test)]
mod tests;
mod view;

use ui::framework::{Fixture, ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::prelude::*;
use ui::widget::Element;

/// The mounted resource carries no external message plaintext into a workspace
/// CRDT. Private external histories remain the broker's account-owned data.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Messages {
    pub schema_version: u64,
}

#[resource(kind = "tilo.messages", label = "Messages")]
impl Messages {
    /// Initialize the version marker without creating contacts or messages.
    #[action(housekeeping)]
    pub fn initialize_record(&mut self) {
        self.schema_version = 1;
    }
}

#[derive(Clone, Effect)]
#[effect(operation = "tilo.messages.view", device_only)]
pub enum MessagesEffect {
    /// Open private account setup without changing a conversation or account.
    #[effect(view, idempotent, label = "messages-setup-accounts")]
    OpenAccountSetup,
    /// Select one exact configured service/account for private setup.
    #[effect(view, idempotent)]
    SelectSetupAccount { service: String, account: String },
    /// Show the native host's short-lived private linking code.
    #[effect(job, sensitive, cancellable, label = "messages-start-link")]
    BeginLink { service: String, account: String },
    /// Confirm the displayed attempt on the explicitly selected account.
    #[effect(job, sensitive, cancellable, label = "messages-finish-link")]
    CompleteLink {
        service: String,
        account: String,
        attempt: String,
    },
    /// Link a first account without manufacturing a configured account row.
    #[effect(job, sensitive, cancellable, label = "messages-first-account-prepare")]
    BeginFirstEnrollment { service: String },
    /// Open the host's private SIM chooser without granting modem access.
    #[effect(job, sensitive, cancellable, idempotent, label = "messages-first-sms")]
    ListSmsSims,
    /// Review one opaque host choice; bus paths and SIM identifiers are not inputs.
    #[effect(job, sensitive, cancellable, label = "messages-select-sim")]
    SelectSmsSim { selection: String },
    /// Verify the exact unpublished pairing and review its self identity.
    #[effect(job, sensitive, cancellable, label = "messages-finish-link")]
    CompleteFirstEnrollment {
        service: String,
        account: String,
        attempt: String,
    },
    /// Confirm self identity and propose access for protected Passwords review.
    #[effect(job, sensitive, cancellable, label = "messages-first-confirm")]
    ReviewFirstEnrollment {
        service: String,
        account: String,
        display_name: String,
    },
    /// Retire an interrupted unpublished attempt and return to service selection.
    #[effect(view, idempotent, label = "messages-first-reset")]
    ResetFirstEnrollment,
    /// Interrupt only this account's private provisioning attempt.
    #[effect(view, idempotent, label = "messages-cancel-link")]
    CancelLink,
    /// List your active Contacts identities.
    #[effect(view, returns, idempotent, label = "messages-list-people")]
    ListPeople,
    /// Non-secret lifecycle for each exact service and sender account.
    #[effect(view, returns, idempotent, label = "messages-list-account-states")]
    ListAccountStates,
    /// List the service and account choices for this person.
    #[effect(view, returns, idempotent, label = "messages-list-routes")]
    ListRoutes { person: String },
    /// Open a person by stable Contacts identity.
    #[effect(view, idempotent)]
    OpenPerson { person: String },
    /// Select an exact service/account/recipient choice, as published by the
    /// current Contacts and account records. Labels are never route keys.
    #[effect(view, idempotent)]
    SelectRoute { choice: String },
    /// Refresh the selected conversation.
    #[effect(
        job,
        cancellable,
        idempotent,
        permission = "read",
        label = "messages-refresh-history"
    )]
    ReadHistory { choice: String },
    /// Return to the person overview.
    #[effect(view, idempotent)]
    Back,
    /// Legacy recipient-only request. Always unavailable: an address without
    /// a provider scope and identity revision cannot confirm a sending route.
    ConfirmRecipient {
        person: String,
        service: String,
        recipient: String,
    },
    /// Review this exact person, association, sender account, provider scope,
    /// recipient and identity revision. This records an address-book decision;
    /// the adapter must separately verify current provider identity before I/O.
    #[effect(destructive)]
    ConfirmAccount { person: String, choice: String },
    /// Send through the currently authorized broker connection. It remains
    /// unavailable until the host supplies that connection; no local append
    /// or delivery state substitutes for a transport operation.
    #[effect(job, sensitive)]
    Send {
        choice: String,
        operation: String,
        #[param(secret)]
        text: String,
    },
}

impl std::fmt::Debug for MessagesEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MessagesEffect({})",
            <Self as ui::framework::Effect>::id(self)
        )
    }
}

pub(crate) static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[("en-US", include_str!("../locales/en-US.ftl"))],
    "messages-empty-heading",
);

pub struct MessagesShell;

impl ResourceApp for MessagesShell {
    type Resource = Messages;
    const ID: &'static str = "so.tilo.messages";
    const TITLE: &'static str = "Messages";
    const ICON: Option<&'static str> = Some("chat");
    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }
    fn view(cx: &mut ViewCx<Messages>) -> Element {
        view::build(cx)
    }
    fn scenarios() -> &'static [&'static str] {
        &[
            "empty",
            "person",
            "person-narrow",
            "history",
            "history-narrow",
        ]
    }
    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        if scenario.is_some_and(|name| name.ends_with("-narrow")) {
            (420, 820)
        } else {
            (1180, 800)
        }
    }
    fn fixture(fixture: &mut Fixture<Messages>) {
        view::fixture(fixture);
    }
}

#[must_use]
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<MessagesShell>()
}

//! Direct messaging: shared Contacts identities, explicit account/recipient
//! choices and separate service histories. Transport state belongs to the
//! privileged broker; it is never inferred from names or widget state.
//!
//! ## What the record holds
//!
//! A person's service histories never enter the workspace record: they stay
//! the broker's account-owned data (`history`). The record holds only
//! conversations put into it on purpose — messages delivered into it and
//! messages queued in it ([`store`]) — and those it holds whole, text
//! included, by design: the person approved giving agents access to message
//! content, and an agent reaches a record only through its published
//! operations, in a worker with no network or account, so what it is to read
//! or answer must be in the record. Everyone the record is shared with reads
//! those conversations; nothing is ever sent from a record operation.

mod enrollment;
mod history;
#[cfg(test)]
mod history_tests;
mod sending;
mod setup;
pub mod store;
#[cfg(test)]
mod tests;
mod view;

use ui::framework::{Fixture, ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::prelude::*;
use ui::widget::Element;

pub use store::{Conversation, Message, Messages, MessagesOperation};

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
    /// Search messages
    ///
    /// The messages the record holds whose sender, recipient, conversation
    /// or text contain every word of the query (any case), newest first,
    /// each with its conversation and message ids for `read`. Changes
    /// nothing and reaches no service.
    #[effect(view, returns, idempotent, permission = "read", device_only = false)]
    SearchMessages {
        #[param("Words every match contains")]
        query: String,
    },
    /// Export conversations as content
    ///
    /// Keeps the record's conversations — all of them or one — in the
    /// content store as a transcript, with no file to write: the caller
    /// reads the bytes by the content address it answers.
    #[effect(content, permission = "read", device_only = false)]
    ExportContent {
        #[param("The format to export", choices = ["md", "json"])]
        format: String,
        #[param("Only this conversation (its id or messages.conversation:<id>)")]
        conversation: Option<String>,
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

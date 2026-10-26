//! Direct messaging: shared Contacts identities, explicit account/recipient
//! choices and separate service histories. Transport state belongs to the
//! privileged broker; it is never inferred from names or widget state.

pub mod conversations;
mod enrollment;
mod groups;
mod history;
#[cfg(test)]
mod history_tests;
mod sending;
mod setup;
#[cfg(test)]
mod tests;
mod view;
mod window_bar;

use ui::framework::{Fixture, ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::prelude::*;
use ui::widget::Element;

/// The record holds only conversations put into it on purpose (delivered
/// or queued, see [`conversations`]); private external histories remain the
/// broker's account-owned data, reached through effects.
pub use conversations::Messages;

#[derive(Clone, Effect)]
#[effect(operation = "tilo.messages.view")]
pub enum MessagesEffect {
    /// Inspect private service groups for one explicitly selected sender account.
    #[effect(
        job,
        sensitive,
        returns,
        cancellable,
        kind = "read",
        label = "messages-list-groups"
    )]
    ListGroups { service: String, account: String },
    /// Fetch current service membership; this does not authorize a group send.
    #[effect(
        job,
        sensitive,
        returns,
        cancellable,
        kind = "read",
        label = "messages-read-group-membership"
    )]
    ReadGroupMembership {
        service: String,
        account: String,
        group: String,
    },
    /// Read private text only for this selected service group.
    #[effect(
        job,
        sensitive,
        returns,
        cancellable,
        kind = "read",
        label = "messages-group-history"
    )]
    ReadGroupHistory {
        service: String,
        account: String,
        group: String,
    },
    /// Send once using the reviewed membership and original sender account.
    #[effect(job, sensitive, label = "messages-group-send")]
    SendGroupText {
        service: String,
        account: String,
        group: String,
        review: String,
        operation: String,
        #[param(secret)]
        text: String,
    },
    /// Add one explicitly selected current Contacts association.
    #[effect(job, destructive, label = "messages-group-add")]
    AddGroupMember {
        service: String,
        account: String,
        group: String,
        review: String,
        operation: String,
        person: String,
        choice: String,
    },
    #[effect(job, destructive, label = "messages-group-remove")]
    RemoveGroupMember {
        service: String,
        account: String,
        group: String,
        review: String,
        operation: String,
        member: String,
    },
    #[effect(job, destructive, label = "messages-group-leave")]
    LeaveGroup {
        service: String,
        account: String,
        group: String,
        review: String,
        operation: String,
    },
    #[effect(view, idempotent, label = "messages-close-groups")]
    CloseGroups,
    /// Open private account setup without changing a conversation or account.
    #[effect(view, idempotent, label = "messages-setup-accounts")]
    OpenAccountSetup,
    /// Select one exact configured service/account for private setup.
    #[effect(view, idempotent)]
    SelectSetupAccount { service: String, account: String },
    /// Show the native host's short-lived private linking code.
    #[effect(attended, job, sensitive, cancellable, label = "messages-start-link")]
    BeginLink { service: String, account: String },
    /// Confirm the displayed attempt on the explicitly selected account.
    #[effect(job, sensitive, cancellable, label = "messages-finish-link")]
    CompleteLink {
        service: String,
        account: String,
        attempt: String,
    },
    /// Link a first account without manufacturing a configured account row.
    #[effect(
        attended,
        job,
        sensitive,
        cancellable,
        label = "messages-first-account-prepare"
    )]
    BeginFirstEnrollment { service: String },
    /// Open the host's private SIM chooser without granting modem access.
    #[effect(
        attended,
        job,
        sensitive,
        cancellable,
        idempotent,
        label = "messages-first-sms"
    )]
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
        kind = "read",
        label = "messages-refresh-history"
    )]
    ReadHistory { choice: String },
    /// Return to the person overview.
    #[effect(view, idempotent)]
    Back,
    /// Review this exact person, association, sender account, provider scope,
    /// recipient and identity revision. This records an address-book decision;
    /// the adapter must separately verify current provider identity before I/O.
    #[effect(destructive)]
    ConfirmAccount { person: String, choice: String },
    /// Search messages
    ///
    /// The messages kept in this record (delivered into it or queued in
    /// it) holding every word of the query in their sender, recipient,
    /// conversation or text, newest first. Changes nothing.
    #[effect(kind = "read", returns, idempotent, audience = "agents")]
    SearchMessages {
        #[param("The words to find, in any case")]
        query: String,
    },
    /// Export conversations
    ///
    /// Keeps the transcript of one conversation, or of every one, as
    /// content, and answers with its content address. Changes nothing.
    #[effect(kind = "read", returns, idempotent, audience = "agents")]
    ExportContent {
        #[param("md (the default) or json", choices = ["md", "json"])]
        format: Option<String>,
        #[param("The conversation; every one when omitted")]
        conversation: Option<String>,
    },
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

ui::embedded_catalog!(pub(crate) static CATALOG, ["en-US"]);

/// The id of [`open_contacts_action`] on the app's resource.
pub(crate) const OPEN_CONTACTS: &str = "open-contacts";

/// Opening Contacts, where the people Messages lists are added: the
/// workspace opens the app ([`ApplicationLaunch`]), so the empty state's
/// button, search and agents all ask for the same thing.
///
/// [`ApplicationLaunch`]: ui::tilo_actions::remote::ApplicationLaunch
pub(crate) fn open_contacts_action() -> ui::tilo_actions::ActionSpec {
    ui::tilo_actions::remote::ApplicationLaunch::app("so.tilo.contacts").spec(
        OPEN_CONTACTS,
        &CATALOG.t("messages-open-contacts"),
        &CATALOG.t("messages-open-contacts-description"),
    )
}

pub struct MessagesShell;

/// What the binary runs instead of opening a window.
///
/// `--probe-contact-directory-engine VALUE` (the value is ignored) runs the
/// contact directory's self-check. It needs `runtime-engine`, the release
/// build, where that code is in `libtilo_contact_directory_engine.so`; the
/// check proves Messages loads that library and runs it.
const COMMANDS: &[ui::framework::Command] = &[ui::framework::Command {
    option: "--probe-contact-directory-engine",
    run: probe_contact_directory_engine,
}];

#[cfg(feature = "runtime-engine")]
fn probe_contact_directory_engine(_: &str) -> Result<(), String> {
    let report = tilo_contact_directory_engine::self_check()?;
    println!("messages --probe-contact-directory-engine: {report}");
    Ok(())
}

#[cfg(not(feature = "runtime-engine"))]
fn probe_contact_directory_engine(_: &str) -> Result<(), String> {
    Err("the contact directory's engine library needs the `runtime-engine` feature".to_string())
}

impl ResourceApp for MessagesShell {
    type Resource = Messages;
    const ID: &'static str = "so.tilo.messages";
    const TITLE: &'static str = "Messages";
    const ICON: Option<&'static str> = Some("chat");
    fn commands() -> &'static [ui::framework::Command] {
        COMMANDS
    }
    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }
    fn view(cx: &mut ViewCx<Messages>) -> Element {
        view::build(cx)
    }
    /// Open Contacts, where the people you message are added.
    fn local_operations() -> Vec<ui::tilo_actions::ActionSpec> {
        vec![open_contacts_action()]
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
    ui::built_in_definition!(MessagesShell)
}

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::MessagesShell::scenarios() {
            let mut app = TestApp::<super::MessagesShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}

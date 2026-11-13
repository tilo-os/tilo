//! Direct messaging: shared Contacts identities, explicit account/recipient choices and
//! separate service histories.

pub mod conversations;
mod drafts;
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

/// The record holds only conversations put into it on purpose (delivered or queued, see
/// [`conversations`]); private external histories remain the broker's account-owned data,
/// reached through effects.
pub use conversations::Messages;

#[derive(Clone, Effect)]
#[effect(operation = "tilo.messages.view")]
pub enum MessagesEffect {
    #[effect(
        job,
        sensitive,
        returns,
        cancellable,
        kind = "read",
        label = "messages-list-groups"
    )]
    ListGroups { service: String, account: String },
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
    #[effect(job, sensitive, returns, label = "messages-group-send")]
    SendGroupText {
        service: String,
        account: String,
        group: String,
        review: String,
        operation: String,
        #[param(secret)]
        text: String,
    },
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
    #[effect(view, idempotent, label = "messages-setup-accounts")]
    OpenAccountSetup,
    #[effect(view, idempotent)]
    SelectSetupAccount { service: String, account: String },
    #[effect(
        attended,
        job,
        sensitive,
        cancellable,
        returns,
        label = "messages-start-link"
    )]
    BeginLink { service: String, account: String },
    #[effect(job, sensitive, cancellable, returns, label = "messages-finish-link")]
    CompleteLink {
        service: String,
        account: String,
        attempt: String,
    },
    #[effect(
        attended,
        job,
        sensitive,
        cancellable,
        returns,
        label = "messages-first-account-prepare"
    )]
    BeginFirstEnrollment { service: String },
    #[effect(
        attended,
        job,
        sensitive,
        cancellable,
        idempotent,
        returns,
        label = "messages-first-sms"
    )]
    ListSmsSims,
    #[effect(job, sensitive, cancellable, returns, label = "messages-select-sim")]
    SelectSmsSim { selection: String },
    #[effect(job, sensitive, cancellable, returns, label = "messages-finish-link")]
    CompleteFirstEnrollment {
        service: String,
        account: String,
        attempt: String,
    },
    #[effect(job, sensitive, cancellable, returns, label = "messages-first-confirm")]
    ReviewFirstEnrollment {
        service: String,
        account: String,
        display_name: String,
    },
    #[effect(view, idempotent, label = "messages-first-reset")]
    ResetFirstEnrollment,
    #[effect(view, idempotent, label = "messages-cancel-link")]
    CancelLink,
    #[effect(view, returns, idempotent, label = "messages-list-people")]
    ListPeople,
    #[effect(view, returns, idempotent, label = "messages-list-account-states")]
    ListAccountStates,
    #[effect(view, returns, idempotent, label = "messages-list-routes")]
    ListRoutes { person: String },
    #[effect(view, idempotent)]
    OpenPerson { person: String },
    #[effect(view, idempotent)]
    OpenConversation { conversation: String },
    /// Labels are never route keys.
    #[effect(view, idempotent)]
    SelectRoute { choice: String },
    #[effect(
        job,
        cancellable,
        idempotent,
        kind = "read",
        label = "messages-refresh-history"
    )]
    ReadHistory { choice: String },
    #[effect(
        job,
        cancellable,
        idempotent,
        kind = "read",
        label = "messages-load-older"
    )]
    ReadOlderHistory { choice: String },
    #[effect(view, idempotent)]
    Back,
    /// This records an address-book decision; the adapter must separately verify current
    /// provider identity before I/O.
    #[effect(destructive)]
    ConfirmAccount { person: String, choice: String },
    #[effect(kind = "read", returns, idempotent, audience = "agents")]
    SearchMessages {
        #[param("The words to find, in any case")]
        query: String,
    },
    #[effect(kind = "read", returns, idempotent, audience = "agents")]
    ExportContent {
        #[param("md (the default) or json", choices = ["md", "json"])]
        format: Option<String>,
        #[param("The conversation; every one when omitted")]
        conversation: Option<String>,
    },
    #[effect(job, sensitive, returns)]
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

ui::embedded_catalog!(pub(crate) static CATALOG, ["de-DE", "en-US"]);

pub(crate) const OPEN_CONTACTS: &str = "open-contacts";

/// Opening Contacts, where the people Messages lists are added: the workspace opens the app
/// ([`ApplicationLaunch`]), so the empty state's button, search and agents all ask for the
/// same thing.
pub(crate) fn open_contacts_action() -> ui::tilo_actions::ActionSpec {
    ui::tilo_actions::remote::ApplicationLaunch::app("so.tilo.contacts").spec(
        OPEN_CONTACTS,
        &CATALOG.t("messages-open-contacts"),
        &CATALOG.t("messages-open-contacts-description"),
    )
}

pub struct MessagesShell;

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
    const TITLE_KEY: &'static str = "messages-app-title";
    const ICON: Option<&'static str> = Some("chat");

    const PERSON_RECORDS: &'static [ui::framework::PersonKind] = &[
        ui::framework::PersonKind::shared::<tilo_messaging::PersonAccounts>(),
        ui::framework::PersonKind::shared::<tilo_contact_directory::ContactDirectory>(),
    ];
    fn commands() -> &'static [ui::framework::Command] {
        COMMANDS
    }
    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }
    fn view(cx: &mut ViewCx<Messages>) -> Element {
        let people = cx.people();
        cx.switch(&people.revision(), move |cx, _| view::build(cx))
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

ui::reach_tests!(MessagesShell);

/// German is one of the languages the app ships, and it reads in German: the copy comes
/// from `locales/de-DE.ftl`, never from the English source catalogue beside it.
#[cfg(test)]
mod german_window {
    #[test]
    fn a_german_window_reads_the_catalogue() {
        let catalog = &super::CATALOG;
        assert!(
            catalog.languages().contains(&"de-DE"),
            "the app ships German among {:?}",
            catalog.languages()
        );
        catalog.set_locale("de-DE");
        assert_eq!(catalog.t("messages-people"), "Personen");
        catalog.set_locale("en-US");
        assert_eq!(catalog.t("messages-people"), "People");
    }
}

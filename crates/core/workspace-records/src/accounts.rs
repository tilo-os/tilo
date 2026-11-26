//! The workspace's connected accounts, kept where the workspace runs, and the actions a page, a
//! window, a device's mount or an agent asks of them: declared here, once, as
//! [`AccountsAction`].
//!
//! A page has no mail connection and no vault of its own. Adding a mail account from a browser
//! therefore asks the host that runs the workspace: it opens the server's session with the
//! password once, keeps the account and its secret in the workspace's own vault, and answers
//! what it kept. Nothing here carries a secret back: a reading names an account's address and
//! its servers, never its password.
//!
//! The accounts are the host's to keep, and the mail itself is Mail's: the app's connector signs
//! in with the account's secret and keeps what its server holds, so reading, searching and
//! sending are its actions, reached as any app's are.
//!
//! The same actions answer a device that keeps its own accounts, so the page and the device read
//! and change accounts through one vocabulary.
//!
//! A page open in a tab keeps the build it loaded while its server moves on, so every message
//! here reads past a field it does not know: a field is added as an optional one, and an older
//! page reads the newer answer.
//!
//! What this declaration publishes ([`surface`]) is written beside this crate as
//! `accounts-surface.json`: a tilolang view compiles against it, and a crate that does not
//! depend on this one calls the actions through `tilo_resource::bindings!` of that file.

use serde::{Deserialize, Serialize};
use tilo_actions::ActionAddress;
use tilo_actions::remote::RemoteActionTarget;
use tilo_actions::remote::{KEEP_SEALED, KEEP_USES, REMOVE_SEALED};
use tilo_resource::Effect as _;

/// What each accounts action writes besides its own resource, by its id, as the host answering
/// it takes it: its steps in the workspace's vault, then its steps in the record Settings keeps
/// the workspace's accounts in. In the vault, a password typed in is kept, a removed account's
/// sign-in goes, and every sign-in to a server with what the vault keeps writes down that use,
/// the first one included. In the accounts record, an account added or changed is kept
/// (`save-account`), a removed one goes (`remove-account`), and a sign-in says how it went
/// (`report-sync`). An action not named writes nothing besides.
pub const ACCOUNT_STEPS: [(&str, &[&str], &[&str]); 18] = {
    const SAVE: &str = "save-account";
    const REMOVE: &str = "remove-account";
    const REPORT: &str = "report-sync";
    const ADDS: (&[&str], &[&str]) = (&[KEEP_SEALED, KEEP_USES, REMOVE_SEALED], &[SAVE]);
    [
        ("add-mail", ADDS.0, ADDS.1),
        ("add-calendar", ADDS.0, ADDS.1),
        ("begin-sign-in", ADDS.0, ADDS.1),
        ("replace-password", &[KEEP_SEALED], &[]),
        // A sign-in begun on a provider's own page keeps its account and the entry its page
        // leaves in the vault once it finishes, and one finished with a code the same.
        ("begin-cloud-sign-in", &[KEEP_SEALED], &[SAVE]),
        ("submit-sign-in-code", &[KEEP_SEALED], &[SAVE]),
        ("remove", &[REMOVE_SEALED], &[REMOVE]),
        ("add-mail-with-item", &[KEEP_USES, REMOVE_SEALED], &[SAVE]),
        ("discover-calendars", &[KEEP_USES, REMOVE_SEALED], &[SAVE]),
        ("reconnect", &[KEEP_USES], &[REPORT]),
        ("check", &[KEEP_USES], &[REPORT]),
        // Turning an account's calendars on keeps the calendar server's sign-in in the vault
        // and uses the mail account's; turning them off forgets the calendar's.
        (
            "set-services",
            &[KEEP_SEALED, KEEP_USES, REMOVE_SEALED],
            &[SAVE, REMOVE],
        ),
        ("set-calendars", &[REMOVE_SEALED], &[SAVE, REMOVE]),
        ("set-files-mode", &[], &[SAVE]),
        // A connected account keeps its grant in the person's vault, in place of the one an
        // earlier connect of the same account kept.
        ("connect-account", &[KEEP_SEALED, REMOVE_SEALED], &[]),
        // Forgetting one reads its grant once, to give it back to the issuer, and removes it.
        ("forget-connected-account", &[KEEP_USES, REMOVE_SEALED], &[]),
        // A mount asking for its account's sign-in writes down the use, as every sign-in to a
        // server with what the workspace's vault keeps does, and keeps a grant it renewed; what
        // it reports is the record's.
        ("cloud-credential", &[KEEP_USES, KEEP_SEALED], &[]),
        ("report-sync", &[], &[REPORT]),
    ]
};

pub use tilo_actions::remote::ACCOUNT_SECRET_ANSWERS;

/// The provider that answers the workspace's accounts under.
pub const PROVIDER: &str = <AccountsAction as tilo_resource::Effect>::OPERATION;
/// The resource the accounts are actions of.
pub const RESOURCE: &str = "accounts";

/// The accounts' actions, each with the parameters its argument carries. A password or a code
/// typed in travels in as a secret and is never answered back.
#[derive(Clone, tilo_resource::Effect)]
#[effect(operation = "tilo.accounts")]
pub enum AccountsAction {
    /// List mail accounts
    ///
    /// The mail accounts the workspace's vault keeps, as a [`MailAccounts`].
    #[effect(kind = "read")]
    List,
    /// Follow the mail accounts
    ///
    /// The same accounts as `list`, sent again whenever they change, until the caller cancels:
    /// what a window follows so an account added in another one reaches it without asking again.
    #[effect(kind = "read")]
    Watch,
    /// Add a mail account
    ///
    /// Adds a mail account once its server accepted its sign-in, and answers it as a
    /// [`MailAccount`].
    #[effect(kind = "undoable")]
    AddMail {
        #[param("The account's address.")]
        address: String,
        #[param("The name its mail is sent under.", name = "displayName")]
        display_name: Option<String>,
        #[param("The sign-in name, when it is not the address.")]
        username: Option<String>,
        #[param(
            "The IMAP server; left out, the server finds both from the address.",
            name = "incomingHost"
        )]
        incoming_host: Option<String>,
        #[param("The IMAP port.", name = "incomingPort")]
        incoming_port: Option<u16>,
        #[param("The SMTP server.", name = "outgoingHost")]
        outgoing_host: Option<String>,
        #[param("The SMTP port.", name = "outgoingPort")]
        outgoing_port: Option<u16>,
        #[param(
            secret,
            "The account's password or app password: sealed in the workspace's vault, never \
             answered back."
        )]
        password: String,
        #[param("Other names and addresses it sends as, each `Name <address>`.")]
        identities: Option<Vec<String>>,
        #[param("What else of the account the person uses; left out, its mail alone.")]
        services: Option<AccountServices>,
    },
    /// Add a mail account from the vault
    ///
    /// Adds a mail account whose sign-in the workspace's vault already keeps, and answers it as
    /// a [`MailAccount`]. The password is named by reference and the host resolves it, only for
    /// the two servers the item belongs to. It carries no secret, so unlike `add-mail` an agent
    /// acting for the person may ask it.
    #[effect(kind = "undoable")]
    AddMailWithItem {
        #[param("The account's address.")]
        address: String,
        #[param("The name its mail is sent under.", name = "displayName")]
        display_name: Option<String>,
        #[param("The sign-in name, when it is neither the item's nor the address.")]
        username: Option<String>,
        #[param("The IMAP server.", name = "incomingHost")]
        incoming_host: String,
        #[param("The IMAP port.", name = "incomingPort")]
        incoming_port: Option<u16>,
        #[param("The SMTP server.", name = "outgoingHost")]
        outgoing_host: String,
        #[param("The SMTP port.", name = "outgoingPort")]
        outgoing_port: Option<u16>,
        // The reference as Passwords names it (`vault.item:<id>`): a view, an agent and a
        // script spell it the same way, and the vault hands out only a login that belongs to
        // both servers the account names.
        #[param("The vault item that is the account's sign-in, by its reference.")]
        password: String,
        #[param(
            "Why whoever asks needs it, in one sentence their person reads when they are asked \
             whether they may."
        )]
        reason: Option<String>,
    },
    /// Connect a calendar server
    ///
    /// Connects a calendar account at a `CalDAV` server its person names, once that server took
    /// the sign-in, and answers it as a [`MailAccount`]: what a person who rents a calendar from
    /// no mail provider connects.
    #[effect(kind = "undoable")]
    AddCalendar {
        #[param(
            "The CalDAV server's address, such as `dav.example.org`: a bare name is that server \
             over https."
        )]
        server: String,
        #[param("The name it signs in with.")]
        username: String,
        #[param(
            secret,
            "Its password or app password, sealed in the workspace's vault once the server took \
             it and never answered back."
        )]
        password: String,
        #[param("The name the account is shown under.")]
        name: Option<String>,
        #[param(
            "Connect it for reading only: nothing is ever written to its server, until the \
             account is removed.",
            name = "readOnly"
        )]
        read_only: Option<bool>,
    },
    /// Sign an account in again
    ///
    /// Signs the account in again with the entry the workspace's vault keeps for it, through the
    /// same sign-in the account was added with: the accounts reading then says how it went.
    #[effect(kind = "undoable")]
    Reconnect {
        #[param("The account's id, as the accounts list names it.")]
        account: String,
    },
    /// Check an account's server
    ///
    /// Checks that an account's server still takes the sign-in the workspace's vault keeps,
    /// answering the probe's own words `{ "result": "…" }`, or the reason it refused them.
    /// Nothing is written.
    #[effect(kind = "read")]
    Check {
        #[param("The account's id, as the accounts list names it.")]
        account: String,
    },
    /// Replace an account's sign-in
    ///
    /// Replaces an account's saved sign-in once the server took the new one: the vault's entry
    /// is the account's own, under the same reference.
    #[effect(kind = "undoable")]
    ReplacePassword {
        #[param("The account's id, as the accounts list names it.")]
        account: String,
        #[param(
            secret,
            "The password or app password the server took: sealed in the workspace's vault, \
             never answered back."
        )]
        password: String,
    },
    /// Remove an account
    ///
    /// Removes an account and its secret, answering the accounts that remain as a
    /// [`MailAccounts`].
    #[effect(kind = "undoable")]
    Remove {
        #[param("The account's id, as the accounts list names it.")]
        account: String,
    },
    /// Find an account's calendars
    ///
    /// Asks a calendar account's server for the calendars it holds and keeps them on the
    /// account, keeping the choice its person made for the ones it held before: what the review
    /// shows before they choose.
    #[effect(kind = "undoable")]
    DiscoverCalendars {
        #[param("The account's id, as the accounts list names it.")]
        account: String,
    },
    /// Choose an account's calendars
    ///
    /// The others keep their events out of the calendar on this workspace's devices.
    #[effect(kind = "undoable")]
    SetCalendars {
        #[param("The account's id, as the accounts list names it.")]
        account: String,
        #[param("The ids of the calendars that show, as the accounts reading names them.")]
        calendars: Vec<String>,
    },
    /// Find where an address's mail lives
    ///
    /// Worked out from the address alone, answering [`FoundServers`]. Nothing signs in: the
    /// person sees the servers before they type a password for them.
    #[effect(kind = "read")]
    FindServers {
        #[param("The address whose servers to find.")]
        address: String,
    },
    /// Sign in to an account on its provider's page
    ///
    /// Answers where the person's browser goes. The server finishes the sign-in itself when the
    /// provider sends the browser back, keeps the account, and every window following `watch`
    /// sees it arrive; until then [`MailAccounts::signing_in`] names it.
    #[effect(kind = "undoable", returns = SignInStep)]
    BeginSignIn {
        #[param("The provider: `nextcloud`, `google`, `microsoft`, `icloud` or `other`.")]
        provider: String,
        #[param("The Nextcloud server's address; left out for the others.")]
        server: Option<String>,
        #[param("The name the account is shown under.")]
        name: Option<String>,
        #[param("The address a provider's own page signs in to, for a mail account.")]
        address: Option<String>,
        #[param("Whether its mail shows in Mail.")]
        mail: Option<bool>,
        #[param("Whether its calendars show in Calendar.")]
        calendar: Option<bool>,
        #[param("Whether its files mount in Files.")]
        files: Option<bool>,
        #[param("The account this signs in again, when its sign-in stopped working.")]
        account: Option<String>,
    },
    /// Stop waiting for a sign-in
    ///
    /// Stops waiting for a sign-in begun on a provider's page, or forgets one that failed,
    /// answering the accounts as [`MailAccounts`].
    #[effect(kind = "undoable")]
    CancelSignIn {
        #[param("The sign-in's id, as beginning it answered.", name = "signIn")]
        sign_in: String,
    },
    /// Choose an account's services
    ///
    /// Answers the accounts as [`MailAccounts`].
    #[effect(kind = "undoable")]
    SetServices {
        #[param("The account's id, as the accounts list names it.")]
        account: String,
        #[param("Whether its mail shows, where it is a mail account.")]
        mail: Option<bool>,
        #[param("Whether its calendars show in Calendar.")]
        calendar: Option<bool>,
        #[param("Whether its files mount in Files.")]
        files: Option<bool>,
    },
    /// Read the logins the vault keeps
    ///
    /// Their names, sign-in names, the places each belongs to and the account each signs in
    /// for, never their secrets. An app picks from them the ones its own places name, as Mail's
    /// does for its `imap` and `smtp` servers and Calendar's for its `https` server; an item's
    /// secret comes only from `vault#release`.
    #[effect(kind = "read", returns = MailSignIns)]
    SignIns,
    /// Choose how an account's files are kept
    ///
    /// Whether a cloud-storage account's files stay in sync or are copied into the workspace
    /// once, answering the accounts as [`MailAccounts`].
    #[effect(kind = "undoable")]
    SetFilesMode {
        #[param("The cloud-storage account.")]
        account: String,
        #[param(
            "`sync` to keep its files in sync, `import` to copy them into the workspace once."
        )]
        mode: String,
    },
    /// Read the accounts a cloud mount keeps
    ///
    /// Each account's settings, and whether its person signed in, never its secret (a
    /// [`CloudAccounts`]). Answered only to the person on a connection of their own.
    #[effect(kind = "read")]
    CloudAccounts,
    /// Sign a cloud mount in
    ///
    /// What a device's cloud mount for the account signs in with now, answered as a
    /// [`CloudCredential`] in the receipt alone: its password, or the access token of the grant
    /// the workspace's vault keeps, renewed when it was due or when the mount refused the last
    /// one. The renewed grant is kept in the workspace's vault, never by the mount. Answered only
    /// to the person on a connection of their own.
    #[effect(kind = "read")]
    CloudCredential {
        #[param("The account's id.")]
        account: String,
        #[param("The mount refused the last sign-in, so its grant is renewed once more.")]
        rejected: Option<bool>,
    },
    /// Say how a cloud mount's sync went
    ///
    /// How a cloud mount's sync of the account went on this device, as every member's Settings
    /// reads it: what its own device could not say itself.
    #[effect(kind = "undoable")]
    ReportSync {
        #[param("The account's id.")]
        account: String,
        #[param(
            "How its sync went: `healthy`, `offline`, `needs_sign_in`, `error`, `syncing` or \
             `never`."
        )]
        health: String,
        #[param("Whether its person just signed in to it.", name = "signedIn")]
        signed_in: Option<bool>,
        #[param("A short summary a person reads.")]
        message: Option<String>,
    },
    /// Sync an account now
    ///
    /// Cuts a failure's wait short and tries an account whose mount could not start.
    #[effect(kind = "undoable")]
    SyncNow {
        #[param("The cloud-storage account whose sync it is.")]
        account: String,
    },
    /// Pause an account's sync
    ///
    /// Stops a cloud-storage account's background sync and uploads until it is resumed; reads
    /// still fetch on demand.
    #[effect(kind = "undoable")]
    PauseSync {
        #[param("The cloud-storage account whose sync it is.")]
        account: String,
    },
    /// Resume an account's sync
    ///
    /// Resumes a paused cloud-storage account and syncs it now.
    #[effect(kind = "undoable")]
    ResumeSync {
        #[param("The cloud-storage account whose sync it is.")]
        account: String,
    },
    /// Connect cloud storage
    ///
    /// Answers where the person's browser goes. A sign-in the provider shows a code for, or
    /// iCloud's, is finished with `submit-sign-in-code`; one approved on the provider's page
    /// finishes itself and the accounts reading shows the account arrive.
    #[effect(kind = "undoable", returns = SignInStep)]
    BeginCloudSignIn {
        #[param("The storage: `google-drive`, `onedrive`, `dropbox`, `icloud-drive` or `webdav`.")]
        kind: String,
        #[param("The name the account is shown under.")]
        name: Option<String>,
        #[param("A `WebDAV` server's address.")]
        server: Option<String>,
        #[param("The sign-in name: a `WebDAV` server's, or the Apple ID.")]
        username: Option<String>,
        #[param(
            secret,
            "Its password or app password, used once and never answered back."
        )]
        password: Option<String>,
    },
    /// Finish signing in with a code
    ///
    /// iCloud's verification code, or the code a provider showed instead of redirecting back.
    /// Answers the accounts as [`MailAccounts`] once the account is kept.
    #[effect(kind = "undoable")]
    SubmitSignInCode {
        #[param("The sign-in's id, as beginning it answered.", name = "signIn")]
        sign_in: String,
        #[param(secret, "The code the provider sent or showed.")]
        code: String,
    },
    /// Connect an account at an app's issuer
    ///
    /// Connects the person's account at an issuer an app describes, so the app can tell who
    /// they are there: answered with where their browser goes, and finished with
    /// `finish-connect-account`. Only a person asks it, from that app. The grant stays in their
    /// vault; the app reads back only the account the issuer confirmed, its id and name, and
    /// only for issuers it described itself.
    #[effect(kind = "undoable", returns = SignInStep)]
    ConnectAccount {
        #[param("The issuer the app describes.")]
        issuer: AccountIssuer,
        #[param("The public client the app signs in as there.", name = "clientId")]
        client_id: String,
    },
    /// Wait for a connected account
    ///
    /// Waits for a `connect-account` to come back from the person's browser, answering the
    /// [`ConnectedAccount`] the issuer confirmed, or why it did not, in words the person reads.
    #[effect(kind = "undoable", returns = ConnectedAccount)]
    FinishConnectAccount {
        #[param("The connect's id, as beginning it answered.", name = "signIn")]
        sign_in: String,
    },
    /// Forget a connected account
    ///
    /// Its grant leaves the person's vault and is given back to the issuer where it takes one
    /// back. Only the person asks it.
    #[effect(kind = "undoable")]
    ForgetConnectedAccount {
        #[param("The app that described the account's issuer.")]
        app: String,
        #[param("The issuer's id, as that app names it.")]
        issuer: String,
    },
}

/// Each action's id, as its declaration in [`AccountsAction`] names it.
macro_rules! ids {
    ($($id:ident),* $(,)?) => {
        $(
            #[doc = concat!("The id of [`AccountsAction::", stringify!($id), "`].")]
            pub const $id: &str = AccountsAction::$id.id();
        )*
    };
}

ids! {
    LIST, WATCH, ADD_MAIL, ADD_MAIL_WITH_ITEM, ADD_CALENDAR, RECONNECT, CHECK, REPLACE_PASSWORD, REMOVE,
    DISCOVER_CALENDARS, SET_CALENDARS, FIND_SERVERS, BEGIN_SIGN_IN, CANCEL_SIGN_IN, SET_SERVICES,
    SIGN_INS, SET_FILES_MODE, CLOUD_ACCOUNTS, CLOUD_CREDENTIAL, REPORT_SYNC, SYNC_NOW, PAUSE_SYNC,
    RESUME_SYNC, BEGIN_CLOUD_SIGN_IN, SUBMIT_SIGN_IN_CODE, CONNECT_ACCOUNT, FINISH_CONNECT_ACCOUNT,
    FORGET_CONNECTED_ACCOUNT
}

/// The declaration of the action `action`, when it is one of [`AccountsAction`].
#[must_use]
pub fn declaration(action: &str) -> Option<tilo_resource::EffectDecl> {
    AccountsAction::declarations()
        .into_iter()
        .find(|decl| decl.id == action)
}

/// The parameters the action `action` is published with, where it is one of
/// [`AccountsAction`]: what the device keeps none of where a parameter is a secret.
#[must_use]
pub fn params_of(action: &str) -> Option<Vec<tilo_actions::ActionParam>> {
    declaration(action).map(|decl| decl.params)
}

/// The target of action `action` of [`RESOURCE`].
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// The workspace's accounts as a view of another language reads and changes them
/// (`accounts.accounts`): the reading's fields, the records it holds and the actions a view
/// calls.
#[must_use]
pub fn surface() -> tilo_actions::surface::Surface {
    tilo_resource::surface::reading_surface::<Accounts, AccountsAction>(
        RESOURCE,
        "Connected accounts",
    )
}

/// The id of the grant a device hands an app's own connection for the workspace's accounts:
/// what the app's signed manifest declares by naming [`SIGN_INS`]'s capability, and what lets it
/// read the logins there and nothing else of the accounts.
#[must_use]
pub fn sign_in_grant_id(app: &str, workspace: &str) -> String {
    format!("manifest:{app}:{workspace}:sign-ins")
}

/// Where reading the workspace's sign-ins ([`SIGN_INS`]) stands in its policy: the verb at the
/// accounts Mail keeps. The grant an app's signed manifest declares names this node, and the
/// workspace decides an app's read of the sign-ins by that grant as by any other.
#[must_use]
pub fn sign_ins_node(workspace: &str) -> tilo_action_policy::TreePath {
    tilo_action_policy::TreePath::workspace(workspace.to_owned())
        .app(tilo_app_id::apps::MAIL)
        .kind(PROVIDER)
        .rest()
        .action(SIGN_INS)
}

/// A workspace's accounts, as one reading (`accounts.accounts`).
#[derive(Debug, Serialize, tilo_resource::JsonSchema)]
pub struct Accounts {
    /// The accounts the workspace connected, in the record's order.
    pub accounts: Vec<AccountView>,
    /// The accounts this device's person connected at issuers their apps describe.
    pub connected: Vec<ConnectedAccount>,
}

/// One account the workspace connected, without its secret.
#[derive(Debug, Serialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    /// The account's id, as every action names it.
    pub id: String,
    /// The kind of service it is.
    pub kind: AccountKind,
    /// What its person calls it.
    pub name: String,
    /// The address or name it signs in at.
    pub address: String,
    /// The name it signs in with.
    pub username: String,
    /// The provider it belongs to.
    pub provider: String,
    /// How its last sync went.
    pub status: AccountStatus,
    /// What a person reads about that, when the account's device said something.
    pub words: String,
    /// Whether the account is in use.
    pub enabled: bool,
    /// A mail account's incoming server as a person writes it.
    pub incoming: String,
    /// Its outgoing server as a person writes it.
    pub outgoing: String,
    /// The other addresses a mail account sends as, each `Name <address>`.
    pub identities: Vec<String>,
    /// A calendar account's calendars and which of them show.
    pub calendars: Vec<CalendarView>,
    /// Which of its services the account feeds.
    pub services: AccountServices,
    /// A cloud-storage account's files: `sync` kept in sync, `import` copied into the
    /// workspace once; empty for an account that mounts no files.
    pub files_mode: String,
    /// Whether its background sync and uploads are paused on this device.
    pub paused: bool,
}

/// One of a calendar account's calendars, as its person chooses which show.
#[derive(Debug, Serialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CalendarView {
    /// Its id, as its server keeps it and the selection names it.
    pub id: String,
    /// What its server calls it.
    pub name: String,
    /// Whether its events show in the calendar.
    pub visible: bool,
    /// Whether its server lets this workspace change it.
    pub read_only: bool,
}

/// The kind of service an account is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AccountKind {
    Mail,
    Calendar,
    Cloud,
    Other,
}

/// How an account's last sync went, as a person reads it on the account's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AccountStatus {
    /// Up to date.
    Connected,
    /// Its person has to sign in again.
    NeedsReconnect,
    /// Its last sync failed.
    Error,
    /// Waiting for a connection.
    Waiting,
    /// Syncing now.
    Syncing,
    /// Nothing has synced it yet.
    Never,
}

/// Who provides an account: a provider whose own page signs a person in, one
/// whose sign-in is an app password, or any other mail provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    /// Google: Gmail, Google Calendar and Google Drive, signed in on
    /// Google's page.
    Google,
    /// Microsoft: Outlook.com and Microsoft 365 mail and `OneDrive`, signed
    /// in on Microsoft's page.
    Microsoft,
    /// iCloud: mail, calendars and contacts with an app-specific password.
    Icloud,
    /// A Nextcloud server: files, calendars and contacts, signed in on the
    /// server's own page (Login Flow v2).
    Nextcloud,
    /// Any other mail provider: an address and its password.
    #[default]
    Other,
}

impl Provider {
    /// The provider by the name the wire gives it.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "google" => Self::Google,
            "microsoft" => Self::Microsoft,
            "icloud" => Self::Icloud,
            "nextcloud" => Self::Nextcloud,
            "other" => Self::Other,
            _ => return None,
        })
    }

    /// The name the wire gives it.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Microsoft => "microsoft",
            Self::Icloud => "icloud",
            Self::Nextcloud => "nextcloud",
            Self::Other => "other",
        }
    }

    /// Whether the person signs in on the provider's own page
    /// ([`BEGIN_SIGN_IN`]) rather than with a password.
    #[must_use]
    pub const fn signs_in_on_its_page(self) -> bool {
        matches!(self, Self::Google | Self::Microsoft | Self::Nextcloud)
    }

    /// The services an account of this provider can feed.
    #[must_use]
    pub const fn offers(self) -> AccountServices {
        match self {
            Self::Google | Self::Icloud => AccountServices {
                mail: true,
                calendar: true,
                files: false,
            },
            Self::Microsoft | Self::Other => AccountServices {
                mail: true,
                calendar: false,
                files: false,
            },
            Self::Nextcloud => AccountServices {
                mail: false,
                calendar: true,
                files: true,
            },
        }
    }
}

/// The provider a cloud-storage account belongs to, named by the sign-in
/// that connects it ([`BEGIN_CLOUD_SIGN_IN`]). Its account type is the one
/// its files mount with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CloudKind {
    /// Google Drive.
    GoogleDrive,
    /// Microsoft's cloud storage.
    OneDrive,
    /// Dropbox's cloud storage.
    Dropbox,
    /// iCloud Drive, signed in with an Apple ID and a verification code.
    IcloudDrive,
    /// Any server speaking `WebDAV`, signed in with its own user name and
    /// password.
    Webdav,
}

impl CloudKind {
    /// The account type id its files mount as
    /// ([`accounts::cloud_types`]).
    #[must_use]
    pub const fn type_id(self) -> &'static str {
        match self {
            Self::GoogleDrive => "google-drive",
            Self::OneDrive => "onedrive",
            Self::Dropbox => "dropbox",
            Self::IcloudDrive => "icloud-drive",
            Self::Webdav => "webdav",
        }
    }

    /// What people read it as.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::GoogleDrive => "Google Drive",
            Self::OneDrive => "OneDrive",
            Self::Dropbox => "Dropbox",
            Self::IcloudDrive => "iCloud Drive",
            Self::Webdav => "WebDAV",
        }
    }

    /// Whether the person approves the sign-in on the provider's own page,
    /// in their browser ([`BEGIN_CLOUD_SIGN_IN`] answers where), rather than
    /// typing a password here.
    #[must_use]
    pub const fn signs_in_on_its_page(self) -> bool {
        matches!(self, Self::GoogleDrive | Self::OneDrive | Self::Dropbox)
    }
}

/// Which of an account's services the person uses. One account feeds every
/// app: Mail reads it where `mail` is on, Calendar where `calendar` is.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct AccountServices {
    #[serde(default)]
    pub mail: bool,
    #[serde(default)]
    pub calendar: bool,
    #[serde(default)]
    pub files: bool,
}

impl Default for AccountServices {
    /// A mail account's: its mail.
    fn default() -> Self {
        Self {
            mail: true,
            calendar: false,
            files: false,
        }
    }
}

impl AccountServices {
    /// Only what `provider` offers of these.
    #[must_use]
    pub const fn within(self, provider: Provider) -> Self {
        let offered = provider.offers();
        Self {
            mail: self.mail && offered.mail,
            calendar: self.calendar && offered.calendar,
            files: self.files && offered.files,
        }
    }

    /// Whether none is on.
    #[must_use]
    pub const fn none(self) -> bool {
        !(self.mail || self.calendar || self.files)
    }
}

/// One account, as [`LIST`] and [`ADD_MAIL`] answer it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailAccount {
    /// The account's id, as [`REMOVE`] names it.
    pub id: String,
    pub address: String,
    pub display_name: String,
    pub incoming_host: String,
    pub incoming_port: u16,
    pub outgoing_host: String,
    pub outgoing_port: u16,
    /// Whether the person's vault keeps the account's sign-in. A host
    /// never answers the secret itself.
    pub has_secret: bool,
    /// Other names and addresses it sends as, each `Name <address>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identities: Vec<String>,
    /// Who provides it.
    #[serde(default)]
    pub provider: Provider,
    /// Which of its services the person uses.
    #[serde(default)]
    pub services: AccountServices,
    /// The server a Nextcloud account signs in to, or the calendar server of
    /// another; empty where there is none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub server: String,
}

/// A sign-in begun with [`BEGIN_SIGN_IN`] that has not made an account yet.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SigningIn {
    /// The sign-in's id, as [`CANCEL_SIGN_IN`] names it.
    pub id: String,
    pub provider: Provider,
    /// The Nextcloud server it signs in to; empty for the others.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub server: String,
    /// Why it made no account, once it failed: in words the person reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
}

/// Every mail account the person's vault keeps.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailAccounts {
    pub accounts: Vec<MailAccount>,
    /// The account the person chose last, when the store names one; the
    /// first account otherwise.
    #[serde(default)]
    pub active: String,
    /// The sign-ins begun on a provider's page that made no account yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signing_in: Vec<SigningIn>,
    /// The providers this server can sign in with now, in the order its
    /// page offers them: Google or Microsoft once a web client is
    /// configured for it, Nextcloud always. Only these are worth a button.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signs_in_with: Vec<Provider>,
}

/// One server and its port.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerAt {
    /// The server's name.
    pub name: String,
    pub port: u16,
}

/// Where an address's mail lives ([`FIND_SERVERS`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundServers {
    /// Who provides it. A provider that signs in on its own page
    /// ([`Provider::signs_in_on_its_page`]) is signed in to there, never with
    /// a password.
    pub provider: Provider,
    pub incoming: ServerAt,
    pub outgoing: ServerAt,
    /// The sign-in name, when the provider says it is not the address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

/// A sign-in to begin on a provider's own page ([`BEGIN_SIGN_IN`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeginSignIn {
    /// Google, Microsoft or Nextcloud.
    pub provider: Provider,
    /// The Nextcloud server's address; empty for the others.
    pub server: String,
    /// What the account is used for, said one service at a time: the
    /// provider is asked for only what it offers of them.
    pub services: AccountServices,
    /// The name the account is shown under; its server's or its address
    /// where it is empty.
    pub name: String,
    /// The address the person signs in with, when the provider's page is
    /// asked for a mailbox: what Google's and Microsoft's pages sign in to,
    /// shown to them on that page and kept as the account's address.
    pub address: String,
    /// The account this signs in to again, when its sign-in stopped working;
    /// empty for a new account.
    pub account: String,
}

/// A cloud-storage account to connect ([`BEGIN_CLOUD_SIGN_IN`]).
#[derive(Clone, PartialEq, Eq)]
pub struct BeginCloudSignIn {
    pub kind: CloudKind,
    /// The name the account is shown under; the provider's where it is empty.
    pub name: String,
    /// A `WebDAV` server's address; empty for the providers whose own page
    /// is opened.
    pub server: String,
    /// The sign-in name: a `WebDAV` server's or the `Apple ID`. Empty for the
    /// providers whose own page is opened.
    pub username: String,
    /// Its password or app password, used once for this request and never
    /// kept here: sealed in the workspace's vault once the sign-in holds.
    pub password: String,
}

impl std::fmt::Debug for BeginCloudSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BeginCloudSignIn")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("server", &self.server)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// Where the person's browser goes to sign in, and what the sign-in waits for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SignInStep {
    /// The sign-in's id, as [`MailAccounts::signing_in`] and
    /// [`CANCEL_SIGN_IN`] name it.
    pub id: String,
    /// The provider's page to open.
    pub open: String,
    /// What the sign-in waits for beyond the browser, in words the person
    /// reads: a verification code Apple sent them, or the code a provider
    /// showed instead of redirecting back. Empty when it needs none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
}

/// An issuer an app describes, to connect a person's account there ([`CONNECT_ACCOUNT`]): its
/// endpoints, what it is asked for, which fields of its identity answer name the account, and
/// the one loopback port its client registered `http://127.0.0.1:<port>/oauth` for. Data, never
/// code: every issuer is signed in to the same way.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountIssuer {
    /// Unique among the issuers one app describes.
    pub id: String,
    pub label: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    /// Answers the signed-in account to its access token.
    pub identity_endpoint: String,
    /// Where a grant is given back when the person forgets the account, if the issuer has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation_endpoint: Option<String>,
    /// The identity answer's field holding the account's stable id.
    pub id_field: String,
    /// The identity answer's field holding the name the account goes by.
    pub name_field: String,
    pub scopes: Vec<String>,
    pub redirect_port: u16,
}

/// An account its person connected at an issuer an app describes: who they are there, never
/// what signs them in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectedAccount {
    /// The app that described the issuer.
    pub app: String,
    /// The issuer's id, as that app describes it.
    pub issuer: String,
    /// The issuer's label.
    pub label: String,
    /// The account's id at the issuer.
    pub id: String,
    /// The name the account goes by there.
    pub name: String,
}

/// A login of the workspace's vault, as [`SIGN_INS`] lists it: where it belongs, never what it
/// signs in with.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct MailSignIn {
    /// The item's reference, as [`ADD_MAIL_WITH_ITEM`] names it.
    pub id: String,
    /// What Passwords shows the entry as.
    pub title: String,
    /// The name it signs in with.
    pub username: String,
    /// The places it belongs to, each as the vault keeps one: `imap://<host>:<port>`,
    /// `smtp://<host>:<port>`, `https://<host>` and the like.
    pub places: Vec<String>,
    /// The account of the workspace's it signs in for, where the workspace keeps one for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<SignedInAccount>,
}

/// An account of the workspace's that has a sign-in, as [`SIGN_INS`] shows it to the app that
/// signs it in: where it signs in, never what it signs in with.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct SignedInAccount {
    /// The account's id, as every action names it.
    pub id: String,
    /// The kind of account it is (`caldav`, `email`).
    pub kind: String,
    /// What its person calls it.
    pub name: String,
    /// The address or name it signs in at.
    pub address: String,
    /// The server it signs in to, where its settings name one.
    pub server: String,
    /// Whether nothing is ever written to its server.
    pub read_only: bool,
    /// Whether the account is in use.
    pub enabled: bool,
    /// The account's settings an app that signs in reads: its servers, its ports and the name
    /// it signs in as, and the vault entry that keeps its secret.
    #[serde(default)]
    pub settings: serde_json::Value,
}

/// Every login of the workspace's vault ([`SIGN_INS`]), each with the account it signs in for.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct MailSignIns {
    pub sign_ins: Vec<MailSignIn>,
}

/// One cloud-storage account as a device's mount reads it ([`CLOUD_ACCOUNTS`]): its
/// settings, without its secret.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudAccount {
    /// The account as the workspace keeps it: its id, type and settings, which say
    /// where its mount goes and how.
    pub account: serde_json::Value,
    /// Its person signed in to it, and no device reported a sync since.
    #[serde(default)]
    pub signed_in: bool,
}

/// The cloud-storage accounts one workspace keeps ([`CLOUD_ACCOUNTS`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudAccounts {
    pub accounts: Vec<CloudAccount>,
}

/// What a cloud mount signs in with ([`CLOUD_CREDENTIAL`]): one of a password, an
/// `OAuth` access token, or an iCloud web session's headers. A secret answer, so no surface
/// names it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudCredential {
    pub kind: CloudCredentialKind,
    /// The sign-in name a password belongs to; empty for a token.
    #[serde(default)]
    pub username: String,
    /// The password or token itself. It prints nothing, and nothing keeps it.
    pub secret: String,
    /// A web session's headers, each sent with every request.
    #[serde(default)]
    pub headers: Vec<CloudHeader>,
    /// When a token or session stops being accepted, in milliseconds since the Unix
    /// epoch, where its provider says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
}

impl std::fmt::Debug for CloudCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CloudCredential")
            .field("kind", &self.kind)
            .field("username", &self.username)
            .field("headers", &self.headers.len())
            .field("expires_at_ms", &self.expires_at_ms)
            .finish_non_exhaustive()
    }
}

/// What a cloud mount signs in with ([`CloudCredential`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CloudCredentialKind {
    Basic,
    Bearer,
    Session,
}

/// One header of a cloud mount's web session ([`CloudCredential`]).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudHeader {
    pub name: String,
    /// The header's value, a cookie or a trust token. It prints nothing.
    pub value: String,
}

impl std::fmt::Debug for CloudHeader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CloudHeader")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// A mail account to add. The password travels here and nowhere back: the
/// host keeps it in the person's vault and answers only [`MailAccount`].
#[derive(Clone, PartialEq, Eq)]
pub struct AddMailAccount {
    pub address: String,
    pub display_name: String,
    /// The sign-in name, when it is not the address itself.
    pub username: Option<String>,
    /// The incoming (IMAP) server. Empty: the server finds both from
    /// the address ([`FIND_SERVERS`]).
    pub incoming_host: String,
    pub incoming_port: u16,
    /// The outgoing (SMTP) server. Empty: found with the incoming one.
    pub outgoing_host: String,
    pub outgoing_port: u16,
    /// The account's sign-in. Empty for an account another mail program
    /// moved in whose sign-in the person has not given yet.
    pub password: String,
    /// Other names and addresses it sends as, each `Name <address>`.
    pub identities: Vec<String>,
    /// What the account is used for. An iCloud account's calendars are
    /// kept with the same app password where `calendar` is on.
    pub services: AccountServices,
}

impl AddMailAccount {
    /// The account `asked` adds, where it is [`AccountsAction::AddMail`]: a port it leaves out
    /// is the standard secure one, and services it leaves out are its mail alone.
    #[must_use]
    pub fn asked(asked: AccountsAction) -> Option<Self> {
        let AccountsAction::AddMail {
            address,
            display_name,
            username,
            incoming_host,
            incoming_port,
            outgoing_host,
            outgoing_port,
            password,
            identities,
            services,
        } = asked
        else {
            return None;
        };
        Some(Self {
            address,
            display_name: display_name.unwrap_or_default(),
            username,
            incoming_host: incoming_host.unwrap_or_default(),
            incoming_port: incoming_port.unwrap_or(IMAPS_PORT),
            outgoing_host: outgoing_host.unwrap_or_default(),
            outgoing_port: outgoing_port.unwrap_or(SUBMISSIONS_PORT),
            password,
            identities: identities.unwrap_or_default(),
            services: services.unwrap_or_default(),
        })
    }
}

impl std::fmt::Debug for AddMailAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddMailAccount")
            .field("address", &self.address)
            .field("display_name", &self.display_name)
            .field("username", &self.username)
            .field("incoming_host", &self.incoming_host)
            .field("incoming_port", &self.incoming_port)
            .field("outgoing_host", &self.outgoing_host)
            .field("outgoing_port", &self.outgoing_port)
            .field("services", &self.services)
            .finish_non_exhaustive()
    }
}

/// The port an account's incoming server is asked at where none is named: IMAP over TLS.
const IMAPS_PORT: u16 = 993;

/// The port an account's outgoing server is asked at where none is named: SMTP over TLS.
const SUBMISSIONS_PORT: u16 = 465;

/// A mail account to add with a sign-in the person's vault keeps. Nothing
/// here is a secret: `password` names the vault item, which must be a login
/// that belongs to both servers (`imap://<incoming host>:<port>` and
/// `smtp://<outgoing host>:<port>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddMailWithItem {
    pub address: String,
    pub display_name: String,
    /// The sign-in name, when it is neither the item's own nor the address.
    pub username: Option<String>,
    pub incoming_host: String,
    pub incoming_port: u16,
    pub outgoing_host: String,
    pub outgoing_port: u16,
    /// The vault item that is the account's sign-in.
    pub password: crate::vault::ItemRef,
    /// Why whoever asks needs it, in a sentence its person reads when they
    /// are asked first; empty when nothing was said.
    pub reason: String,
}

impl AddMailWithItem {
    /// The account [`ADD_MAIL_WITH_ITEM`] asks for with `arguments`, read as its declaration
    /// names them; `None` where they do not read, or its password names no vault item.
    #[must_use]
    pub fn asked(arguments: &serde_json::Map<String, serde_json::Value>) -> Option<Self> {
        let Ok(AccountsAction::AddMailWithItem {
            address,
            display_name,
            username,
            incoming_host,
            incoming_port,
            outgoing_host,
            outgoing_port,
            password,
            reason,
        }) = AccountsAction::decode(ADD_MAIL_WITH_ITEM, arguments)
        else {
            return None;
        };
        Some(Self {
            address,
            display_name: display_name.unwrap_or_default(),
            username,
            incoming_host,
            incoming_port: incoming_port.unwrap_or(IMAPS_PORT),
            outgoing_host,
            outgoing_port: outgoing_port.unwrap_or(SUBMISSIONS_PORT),
            password: tilo_actions::remote::ItemRef::parse(&password)?,
            reason: reason.unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    /// The file a tilolang view compiles the workspace's accounts against, and a crate that does
    /// not depend on this one binds its calls to, is what this crate declares.
    /// `UPDATE_SNAPSHOTS=1` writes it.
    #[test]
    fn the_accounts_surface_file_is_what_this_crate_publishes() {
        let text =
            tilo_surface_compile::canonical(&serde_json::json!({ "resources": [surface()] }));
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("accounts-surface.json");
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
            std::fs::write(&path, &text).expect("the surface file is written");
            return;
        }
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some(text.as_str()),
            "crates/core/workspace-records/accounts-surface.json is not what this crate \
             publishes; regenerate it with UPDATE_SNAPSHOTS=1"
        );
    }

    /// What the action registry knows of the accounts without this crate names them as they
    /// are declared here: the provider they answer under, the actions it says write in the vault
    /// and the accounts record, and the ones it says answer a secret.
    #[test]
    fn the_registry_names_the_declared_actions() {
        use tilo_actions::remote::ACCOUNTS_PROVIDER;
        assert_eq!(PROVIDER, ACCOUNTS_PROVIDER);
        for (action, _, _) in ACCOUNT_STEPS {
            assert!(
                declaration(action).is_some(),
                "{action} is declared nowhere"
            );
        }
        for action in ACCOUNT_SECRET_ANSWERS {
            assert!(
                declaration(action).is_some(),
                "{action} is declared nowhere"
            );
        }
        // A secret typed in is declared one, so no record of a call keeps it.
        for (action, secret) in [
            (ADD_MAIL, "password"),
            (ADD_CALENDAR, "password"),
            (REPLACE_PASSWORD, "password"),
            (BEGIN_CLOUD_SIGN_IN, "password"),
            (SUBMIT_SIGN_IN_CODE, "code"),
        ] {
            let params = params_of(action).unwrap();
            let param = params.iter().find(|param| param.name == secret).unwrap();
            assert_eq!(param.ty, tilo_actions::ParamType::Secret, "{action}");
        }
    }

    /// A tab whose page is older than its server reads the server's answer
    /// past a field added since, so updating the server breaks no open tab.
    #[test]
    fn an_older_page_reads_past_a_field_added_since() {
        let mut answered = serde_json::to_value(MailAccounts {
            accounts: vec![MailAccount {
                address: "ada@example.test".into(),
                ..MailAccount::default()
            }],
            ..MailAccounts::default()
        })
        .unwrap();
        answered["accounts"][0]["addedLater"] = serde_json::json!("anything");
        answered["addedLater"] = serde_json::json!(1);
        let read: MailAccounts = serde_json::from_value(answered).unwrap();
        assert_eq!(read.accounts[0].address, "ada@example.test");

        // No message of the accounts refuses what it does not know.
        let source = include_str!("accounts.rs");
        assert!(!source.contains(concat!("deny_unknown", "_fields")));
    }

    /// A password never reaches a log: the account read for a person hides
    /// it, and the reading the host answers has no field to carry one.
    #[test]
    fn nothing_read_back_carries_the_sign_in() {
        let adding = AddMailAccount {
            address: "ada@example.com".into(),
            display_name: "Ada".into(),
            username: None,
            incoming_host: "imap.example.com".into(),
            incoming_port: 993,
            outgoing_host: "smtp.example.com".into(),
            outgoing_port: 465,
            password: "hunter2".into(),
            identities: Vec::new(),
            services: AccountServices::default(),
        };
        let printed = format!("{adding:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(printed.contains("imap.example.com"), "{printed}");

        let read = MailAccount {
            id: "ada@example.com".into(),
            address: "ada@example.com".into(),
            display_name: "Ada".into(),
            incoming_host: "imap.example.com".into(),
            incoming_port: 993,
            outgoing_host: "smtp.example.com".into(),
            outgoing_port: 465,
            has_secret: true,
            identities: Vec::new(),
            ..Default::default()
        };
        let json = serde_json::to_value(&read).unwrap();
        assert!(json.get("password").is_none());
        assert_eq!(json["hasSecret"], serde_json::json!(true));
    }

    /// An account added without a port signs in over the standard secure
    /// ports; a caller that names none is not refused.
    #[test]
    fn a_mail_account_defaults_to_the_secure_ports() {
        let asked = AccountsAction::decode(
            ADD_MAIL,
            serde_json::json!({
                "address": "ada@example.com",
                "incomingHost": "imap.example.com",
                "outgoingHost": "smtp.example.com",
                "password": "hunter2",
            })
            .as_object()
            .unwrap(),
        )
        .unwrap();
        let adding = AddMailAccount::asked(asked).unwrap();
        assert_eq!(adding.incoming_port, 993);
        assert_eq!(adding.outgoing_port, 465);
    }
}

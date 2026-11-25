//! The person's mail accounts, kept where their workspace runs.
//!
//! A page has no mail connection and no vault of its own. Adding a mail
//! account from a browser therefore asks the host that runs the workspace:
//! it opens the server's session with the password once, keeps the account
//! and its secret in the workspace's own vault, and answers what it kept.
//! Nothing here carries a secret back: a reading names an account's address
//! and its servers, never its password.
//!
//! The accounts are the host's to keep, and the mail itself is Mail's: the
//! app's connector signs in with the account's secret and keeps what its
//! server holds, so reading, searching and sending are its actions, reached
//! as any app's are.
//!
//! The same actions answer a device that keeps its own accounts, so the page
//! and the device read and change accounts through one vocabulary.
//!
//! A page open in a tab keeps the build it loaded while its server moves
//! on, so every message here reads past a field it does not know: a field
//! is added as an optional one, and an older page reads the newer answer.

use serde::{Deserialize, Serialize};

use super::{ActionAddress, RemoteActionTarget};

/// The provider that answers the person's mail accounts under.
pub const PROVIDER: &str = "tilo.accounts";
/// The resource the accounts are actions of.
pub const RESOURCE: &str = "accounts";
/// The mail accounts the person's vault keeps, as a [`MailAccounts`].
pub const LIST: &str = "list";
/// Live: the same accounts as [`LIST`], sent again whenever they change,
/// until the caller cancels. What a window follows so an account added in
/// another one reaches it without asking again.
pub const WATCH: &str = "watch";
/// Add a mail account, once its server accepted its sign-in
/// ([`AddMailAccount`]), and answer it as a [`MailAccount`].
pub const ADD_MAIL: &str = "add-mail";
/// Add a mail account whose sign-in the person's vault already keeps
/// ([`AddMailWithItem`]), and answer it as a [`MailAccount`]. The password is
/// named by reference and the host resolves it, only for the two servers the
/// item belongs to. It carries no secret, so unlike [`ADD_MAIL`] an agent
/// acting for the person may ask it.
pub const ADD_MAIL_WITH_ITEM: &str = "add-mail-with-item";
/// Sign an account in again (`account`) with the entry the workspace's vault
/// keeps for it, through the same sign-in the account was added with: the
/// accounts reading then says how it went.
pub const RECONNECT: &str = "reconnect";
/// Check that an account's server still takes the sign-in the workspace's vault
/// keeps (`account`), answering the probe's own words `{ "result": "…" }`, or
/// the reason it refused them. Nothing is written.
pub const CHECK: &str = "check";
/// Replace an account's saved sign-in ([`ReplacePassword`]) once the server took
/// the new one: the vault's entry is the account's own, under the same reference.
pub const REPLACE_PASSWORD: &str = "replace-password";
/// Remove a mail account and its secret (`account`), answering the accounts
/// that remain as a [`MailAccounts`].
pub const REMOVE: &str = "remove";
/// Ask a calendar account's server for the calendars it holds (`account`) and
/// keep them on the account, keeping the choice its person made for the ones it
/// held before. What the review shows before they choose.
pub const DISCOVER_CALENDARS: &str = "discover-calendars";
/// Say which of a calendar account's calendars show ([`SetCalendars`]): the
/// others keep their events out of the calendar on this workspace's devices.
pub const SET_CALENDARS: &str = "set-calendars";
/// Where the mail of an address lives ([`FindServers`]), worked out from the
/// address alone, answering [`FoundServers`]. Nothing signs in: the person
/// sees the servers before they type a password for them.
pub const FIND_SERVERS: &str = "find-servers";
/// Begin signing in on a provider's own page ([`BeginSignIn`]), answering
/// where the person's browser goes ([`SignInStep`]). The server finishes the
/// sign-in itself when the provider sends the browser back, keeps the
/// account, and every window following [`WATCH`] sees it arrive; until then
/// [`MailAccounts::signing_in`] names it.
pub const BEGIN_SIGN_IN: &str = "begin-sign-in";
/// Stop waiting for a sign-in begun with [`BEGIN_SIGN_IN`] (`signIn`), or
/// forget one that failed, answering the accounts as [`MailAccounts`].
pub const CANCEL_SIGN_IN: &str = "cancel-sign-in";
/// Say which of an account's services the person uses ([`SetServices`]),
/// answering the accounts as [`MailAccounts`].
pub const SET_SERVICES: &str = "set-services";
/// Read the logins the workspace's vault keeps, as a [`MailSignIns`]: their names, sign-in
/// names and the places each belongs to, never their secrets. An app picks from them the ones
/// its own places name, as Mail's does for its `imap` and `smtp` servers; an item's secret
/// comes only from `vault#release`.
pub const SIGN_INS: &str = "sign-ins";
/// Say whether a cloud-storage account's files stay in sync or are copied into
/// the workspace once ([`SetFilesMode`]), answering the accounts as
/// [`MailAccounts`].
pub const SET_FILES_MODE: &str = "set-files-mode";
/// Sync a cloud-storage account now, cutting a failure's wait short and trying
/// an account whose mount could not start ([`SyncControl`]).
pub const SYNC_NOW: &str = "sync-now";
/// Stop a cloud-storage account's background sync and uploads until it is
/// resumed; reads still fetch on demand ([`SyncControl`]).
pub const PAUSE_SYNC: &str = "pause-sync";
/// Resume a paused cloud-storage account and sync it now ([`SyncControl`]).
pub const RESUME_SYNC: &str = "resume-sync";
/// Begin connecting a cloud-storage account ([`BeginCloudSignIn`]),
/// answering where the person's browser goes ([`SignInStep`]). A sign-in
/// the provider shows a code for, or iCloud's, is finished with
/// [`SUBMIT_SIGN_IN_CODE`]; one approved on the provider's page finishes
/// itself and the accounts reading shows the account arrive.
pub const BEGIN_CLOUD_SIGN_IN: &str = "begin-cloud-sign-in";
/// Finish a sign-in that waits for a code ([`SubmitSignInCode`]): iCloud's
/// verification code, or the code a provider showed instead of redirecting
/// back. Answers the accounts as [`MailAccounts`] once the account is kept.
pub const SUBMIT_SIGN_IN_CODE: &str = "submit-sign-in-code";
/// The kind of the record Settings keeps the workspace's accounts in, without their secrets.
pub const ACCOUNTS_RECORD_KIND: &str = "tilo.accounts";
/// The accounts record's step that keeps an account, added or changed.
pub const SAVE_ACCOUNT: &str = "save-account";
/// The accounts record's step that removes an account.
pub const REMOVE_ACCOUNT: &str = "remove-account";
/// The accounts record's step that says how an account's sign-in or sync went.
pub const REPORT_SYNC: &str = "report-sync";

/// The step `verb` of the record Settings keeps the workspace's accounts in.
fn accounts_step(verb: &str) -> crate::ActionEffect {
    crate::ActionEffect::new(tilo_app_id::apps::SETTINGS, ACCOUNTS_RECORD_KIND, verb)
}

/// What the action `action` of `resource` ([`RESOURCE`]) writes
/// besides its own resource, as the host answering it takes it. In the workspace's vault: a
/// password typed in is kept, a removed account's sign-in goes, and every sign-in to a server
/// with what the vault keeps writes down that use, the first one included. In the accounts record: an account added or
/// changed is kept, a removed one goes, and a sign-in says how it went. The action's own
/// declaration, read wherever the action is listed and wherever a device reads those records.
#[must_use]
pub fn effects(resource: &str, action: &str) -> Vec<crate::ActionEffect> {
    use super::person_vault::{KEEP_SEALED, KEEP_USES, REMOVE_SEALED, vault_step};
    let (vault, accounts): (&[&str], &[&str]) = match (resource, action) {
        (RESOURCE, ADD_MAIL | BEGIN_SIGN_IN) => {
            (&[KEEP_SEALED, KEEP_USES, REMOVE_SEALED], &[SAVE_ACCOUNT])
        }
        (RESOURCE, REPLACE_PASSWORD) => (&[KEEP_SEALED], &[]),
        (RESOURCE, REMOVE) => (&[REMOVE_SEALED], &[REMOVE_ACCOUNT]),
        (RESOURCE, ADD_MAIL_WITH_ITEM | DISCOVER_CALENDARS) => {
            (&[KEEP_USES, REMOVE_SEALED], &[SAVE_ACCOUNT])
        }
        (RESOURCE, RECONNECT | CHECK) => (&[KEEP_USES], &[REPORT_SYNC]),
        // Turning an account's calendars on keeps the calendar server's sign-in in the
        // vault and uses the mail account's; turning them off forgets the calendar's.
        (RESOURCE, SET_SERVICES) => (
            &[KEEP_SEALED, KEEP_USES, REMOVE_SEALED],
            &[SAVE_ACCOUNT, REMOVE_ACCOUNT],
        ),
        (RESOURCE, SET_CALENDARS) => (&[REMOVE_SEALED], &[SAVE_ACCOUNT, REMOVE_ACCOUNT]),
        (RESOURCE, SET_FILES_MODE) => (&[], &[SAVE_ACCOUNT]),
        _ => (&[], &[]),
    };
    vault
        .iter()
        .map(|verb| vault_step(verb))
        .chain(accounts.iter().map(|verb| accounts_step(verb)))
        .collect()
}

/// The target of action `action` of [`RESOURCE`].
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
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
        serde_json::from_value(serde_json::Value::String(key.to_owned())).ok()
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
    pub const fn offers(self) -> Services {
        match self {
            Self::Google | Self::Icloud => Services {
                mail: true,
                calendar: true,
                files: false,
            },
            Self::Microsoft | Self::Other => Services {
                mail: true,
                calendar: false,
                files: false,
            },
            Self::Nextcloud => Services {
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Services {
    #[serde(default)]
    pub mail: bool,
    #[serde(default)]
    pub calendar: bool,
    #[serde(default)]
    pub files: bool,
}

impl Default for Services {
    /// A mail account's: its mail.
    fn default() -> Self {
        Self {
            mail: true,
            calendar: false,
            files: false,
        }
    }
}

impl Services {
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
    pub services: Services,
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

/// An address whose servers to find ([`FIND_SERVERS`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindServers {
    pub address: String,
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginSignIn {
    /// Google, Microsoft or Nextcloud.
    pub provider: Provider,
    /// The Nextcloud server's address; empty for the others.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub server: String,
    /// What the account is used for, said one service at a time: the
    /// provider is asked for only what it offers of them.
    #[serde(default)]
    pub mail: bool,
    #[serde(default)]
    pub calendar: bool,
    #[serde(default)]
    pub files: bool,
    /// The name the account is shown under; its server's or its address
    /// where it is empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The address the person signs in with, when the provider's page is
    /// asked for a mailbox: what Google's and Microsoft's pages sign in to,
    /// shown to them on that page and kept as the account's address.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub address: String,
    /// The account this signs in to again, when its sign-in stopped working;
    /// empty for a new account.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub account: String,
}

impl BeginSignIn {
    /// What it says the account feeds.
    #[must_use]
    pub const fn services(&self) -> Services {
        Services {
            mail: self.mail,
            calendar: self.calendar,
            files: self.files,
        }
    }
}

/// A cloud-storage account to connect ([`BEGIN_CLOUD_SIGN_IN`]).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginCloudSignIn {
    pub kind: CloudKind,
    /// The name the account is shown under; the provider's where it is empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// A `WebDAV` server's address; empty for the providers whose own page
    /// is opened.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub server: String,
    /// The sign-in name: a `WebDAV` server's or the `Apple ID`. Empty for the
    /// providers whose own page is opened.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    /// Its password or app password, used once for this request and never
    /// kept here: sealed in the workspace's vault once the sign-in holds.
    #[serde(default, skip_serializing_if = "String::is_empty")]
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

/// Where the person's browser goes to sign in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

/// A sign-in that waits for a code ([`SUBMIT_SIGN_IN_CODE`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitSignInCode {
    pub sign_in: String,
    pub code: String,
}

/// A sign-in to stop waiting for ([`CANCEL_SIGN_IN`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelSignIn {
    pub sign_in: String,
}

/// The services one account feeds ([`SET_SERVICES`]), said one by one: a
/// view passes what its person chose for each, and the host keeps only what
/// the account's provider offers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetServices {
    pub account: String,
    #[serde(default)]
    pub mail: bool,
    #[serde(default)]
    pub calendar: bool,
    #[serde(default)]
    pub files: bool,
}

impl SetServices {
    /// What it says the account feeds.
    #[must_use]
    pub const fn services(&self) -> Services {
        Services {
            mail: self.mail,
            calendar: self.calendar,
            files: self.files,
        }
    }
}

/// One login of the workspace's vault, as [`SIGN_INS`] lists it: where it belongs, never what
/// it signs in with.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
}

/// Every login of the workspace's vault ([`SIGN_INS`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailSignIns {
    pub sign_ins: Vec<MailSignIn>,
}

/// Whether one cloud-storage account's files stay in sync or are copied into
/// the workspace once ([`SET_FILES_MODE`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetFilesMode {
    pub account: String,
    /// `sync` or `import`.
    pub mode: String,
}

/// One cloud-storage account whose sync is controlled ([`SYNC_NOW`],
/// [`PAUSE_SYNC`], [`RESUME_SYNC`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncControl {
    pub account: String,
}

/// A mail account to add. The password travels here and nowhere back: the
/// host keeps it in the person's vault and answers only [`MailAccount`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddMailAccount {
    pub address: String,
    #[serde(default)]
    pub display_name: String,
    /// The sign-in name, when it is not the address itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// The incoming (IMAP) server. Empty: the server finds both from
    /// the address ([`FIND_SERVERS`]).
    #[serde(default)]
    pub incoming_host: String,
    #[serde(default = "default_imaps_port")]
    pub incoming_port: u16,
    /// The outgoing (SMTP) server. Empty: found with the incoming one.
    #[serde(default)]
    pub outgoing_host: String,
    #[serde(default = "default_submissions_port")]
    pub outgoing_port: u16,
    /// The account's sign-in. Empty for an account another mail program
    /// moved in whose sign-in the person has not given yet.
    #[serde(default)]
    pub password: String,
    /// Other names and addresses it sends as, each `Name <address>`.
    #[serde(default)]
    pub identities: Vec<String>,
    /// What the account is used for. An iCloud account's calendars are
    /// kept with the same app password where `calendar` is on.
    #[serde(default)]
    pub services: Services,
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

const fn default_imaps_port() -> u16 {
    993
}

const fn default_submissions_port() -> u16 {
    465
}

/// A mail account to add with a sign-in the person's vault keeps. Nothing
/// here is a secret: `password` names the vault item, which must be a login
/// that belongs to both servers (`imap://<incoming host>:<port>` and
/// `smtp://<outgoing host>:<port>`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddMailWithItem {
    pub address: String,
    #[serde(default)]
    pub display_name: String,
    /// The sign-in name, when it is neither the item's own nor the address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    pub incoming_host: String,
    #[serde(default = "default_imaps_port")]
    pub incoming_port: u16,
    pub outgoing_host: String,
    #[serde(default = "default_submissions_port")]
    pub outgoing_port: u16,
    /// The vault item that is the account's sign-in.
    pub password: super::person_vault::ItemRef,
    /// Why whoever asks needs it, in a sentence its person reads when they
    /// are asked first; empty when nothing was said.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

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

        // No message of a person's accounts refuses what it does not know.
        let source = include_str!("person_accounts.rs");
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
            services: Services::default(),
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
        let adding: AddMailAccount = serde_json::from_value(serde_json::json!({
            "address": "ada@example.com",
            "incomingHost": "imap.example.com",
            "outgoingHost": "smtp.example.com",
            "password": "hunter2",
        }))
        .unwrap();
        assert_eq!(adding.incoming_port, 993);
        assert_eq!(adding.outgoing_port, 465);
    }
}

/// A new sign-in for an account whose saved one no longer works: the account's id and the
/// password or app password its server took.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplacePassword {
    pub account: String,
    pub password: String,
}

impl std::fmt::Debug for ReplacePassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplacePassword")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

/// Which of a calendar account's calendars show: the account's id and the ids
/// of the calendars it keeps on show, as the accounts reading names them. The
/// calendars left out are kept, with their events out of the calendar.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCalendars {
    pub account: String,
    #[serde(default)]
    pub calendars: Vec<String>,
}

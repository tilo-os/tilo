//! A page's logins: the engine finds the forms and reads what a person typed into one, and the
//! workspace's vault keeps the logins. A window's page asks both, so a password goes between the
//! engine and the vault and nowhere else: never into the app's actions, its records or a note.
//!
//! The site every vault call names is the one the engine says the page is at, read in the same
//! status the session asks for the page's document; the app's own word for it only starts that
//! status. So a login is handed only for the site the page is at, and the engine fills it only
//! while the page is still there.

use tilo_webview::{CredentialAction, ReleasedText};

/// What the app asks of its page's logins: one of the engine's own credential actions, or one the
/// vault answers.
#[derive(Debug, Clone, PartialEq)]
pub enum LoginAsk {
    /// The engine's own: find the forms, read what was typed, fill an address.
    Engine(CredentialAction),
    /// The logins the vault keeps for the page's site.
    List,
    /// Keep what the person typed into a form, the capture the engine took of it, as a new login.
    Save { capture: String },
    /// Keep it in place of the login `item` names.
    Update { capture: String, item: String },
    /// Fill the form `form` with the login `item` names, for the running call `asked_by` names.
    Fill {
        form: String,
        item: String,
        asked_by: Option<String>,
    },
    /// Fill the field `element` with the secret of the login `item` names, straight from the
    /// vault into the page: the engine writes it only while its own document is at the site the
    /// vault released it for. `asked_by` is the running call of the app's action that asked for
    /// it, as the app's runtime named it: the vault decides the fill as whoever made that call.
    FillField {
        element: String,
        item: String,
        asked_by: Option<String>,
    },
    /// Forget the login `item` names.
    Delete { item: String },
}

impl LoginAsk {
    /// The word the app's note names the action by.
    #[must_use]
    pub fn did(&self) -> &'static str {
        match self {
            Self::Engine(action) => action.name(),
            Self::List => "list",
            Self::Save { .. } => "save",
            Self::Update { .. } => "update",
            Self::Fill { .. } => "fill",
            Self::FillField { .. } => "fill_field",
            Self::Delete { .. } => "delete",
        }
    }
}

/// One login the vault keeps for the page's site: what is safe to say of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultLogin {
    /// The item's reference (`vault.item:<id>`), which filling, replacing and forgetting name.
    pub item: String,
    pub title: String,
    pub username: String,
    /// Whether the app kept it itself, so filling it asks nobody.
    pub own: bool,
}

/// What a vault call answers, once it has: its refusal in the person's words, or what it said.
pub type Answered<T> = Box<dyn FnOnce(Result<T, String>) + Send>;

/// The workspace's vault, as a window asks it for its page's logins on the window's own
/// connection. Each call answers through `done`, from any thread, once the vault has; whoever
/// implements it wakes the window then.
pub trait LoginVault {
    /// The logins the vault keeps for `site`.
    fn logins(&self, site: &str, done: Answered<Vec<VaultLogin>>);
    /// Keep `password`, signing in as `username` at `site`, in place of `replacing` if given.
    fn keep(
        &self,
        site: &str,
        username: &str,
        password: ReleasedText,
        replacing: Option<&str>,
        done: Answered<()>,
    );
    /// Forget the login `item` names, answering the logins that remain for `site`.
    fn forget(&self, site: &str, item: &str, done: Answered<Vec<VaultLogin>>);
    /// The name and password of the login `item` names, for a page at `site`, decided as
    /// whoever made the running call `asked_by` names as well as the app.
    fn fill(
        &self,
        site_and_item: (&str, &str),
        asked_by: Option<&str>,
        done: Answered<(String, ReleasedText)>,
    );
}

/// What a vault call answered, for the session to carry on with on the window's thread.
pub(crate) enum VaultAnswer {
    /// The logins of a site, for the action `did`.
    Listed {
        did: &'static str,
        message: &'static str,
        result: Result<Vec<VaultLogin>, String>,
    },
    /// A login kept, for the action `did`, at `site`.
    Kept {
        did: &'static str,
        site: String,
        result: Result<(), String>,
    },
    /// A login handed over to fill `form` of the document `document` at `site`.
    Filled {
        form: String,
        site: String,
        document: String,
        result: Result<(String, ReleasedText), String>,
    },
    /// A login's secret handed over to fill the field `element` of a page at `site`.
    FieldReleased {
        element: String,
        site: String,
        result: Result<(String, ReleasedText), String>,
    },
}

/// What a page that has no vault to ask answers: a private window is offered no saved logins.
pub(crate) const PRIVATE: &str = "A private window is not offered your saved logins";
/// What a window that cannot reach the workspace's vault answers.
pub(crate) const NO_VAULT: &str = "This window cannot reach your workspace's vault, so it \
                                   neither offers nor keeps logins";

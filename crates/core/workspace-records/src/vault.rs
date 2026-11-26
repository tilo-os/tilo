//! What the workspace keeps to sign in with, in its vault on the host it runs on, and the
//! actions a page, a window, an app's process or an agent asks of it: declared here, once, as
//! [`VaultAction`].
//!
//! A login, a key, a code generator's seed, an ssh key or a plain value such as a card number
//! is an item of the workspace's vault, bound to where it belongs: a site, a service, a server.
//! A secret travels in with [`ADD`] and never back, and so do a note and the values of the
//! fields kept with it: a listing says an item's title, kind, tags, its fields' labels, whether
//! it keeps a note, where it belongs, when it was added, changed and last used, and whether a
//! login's password is weak or reused, and nothing else.
//!
//! Passwords is the person's window on the vault: it lists, adds and removes items, reads their
//! uses and takes leaves back, as its own person on their own device. A secret leaves the vault
//! for that person alone, and only one at a time: [`COPY`] puts it on their clipboard, which
//! forgets it again, and [`REVEAL`] answers it to be shown. Each is written down as a use, and no
//! record of the call keeps it.
//!
//! What an agent has is a reference ([`ItemRef`]): the address of an item, which is not its
//! secret. An action that needs a secret takes one as a parameter
//! ([`tilo_actions::remote::item_param`]). Whatever signs in resolves it on the server, where the
//! secret is used, and only at a place the item belongs to; each use is remembered with when,
//! where, for whom and what for ([`USES`]), never with what was handed.
//!
//! A use made with nobody watching, by an agent that works for the person on their server, asks
//! the person first, in the conversation the agent works in. Their "Always" is a leave kept with
//! the item: for that agent, that use and those places. [`USES`] lists the leaves beside the
//! uses, and [`TAKE_BACK`] takes them back.
//!
//! What this declaration publishes ([`surface`]) is written beside this crate as
//! `vault-surface.json`: a crate that does not depend on this one calls the actions through
//! `tilo_resource::bindings!` of that file.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    ACCOUNTS_PROVIDER, CatalogueRow, KEEP_SEALED, KEEP_USES, MODELS_PROVIDER, REMOVE_SEALED,
    RemoteActionTarget, VAULT_KIND, VAULT_RESOURCE,
};
use tilo_resource::Effect as _;

pub use tilo_actions::remote::{ItemRef, VAULT_ITEM_KIND as ITEM_KIND};

/// What each of the vault's actions writes, by its id, as the host answering it takes it: the
/// step it takes in the record the workspace keeps its vault in ([`VAULT_KIND`] in Passwords).
/// An item kept is sealed, one forgotten goes, and every use of a secret, a copy and a reveal
/// included, is written down. A read writes nothing, and an action not named writes nothing.
pub const VAULT_STEPS: [(&str, &[&str]); 13] = [
    ("add", &[KEEP_SEALED]),
    ("ask", &[KEEP_SEALED]),
    ("keep", &[KEEP_SEALED]),
    ("keep-page-login", &[KEEP_SEALED]),
    ("remove", &[REMOVE_SEALED]),
    ("forget-page-login", &[REMOVE_SEALED]),
    ("copy", &[KEEP_USES]),
    ("reveal", &[KEEP_USES]),
    ("fill-page", &[KEEP_USES]),
    ("page-fill", &[KEEP_USES]),
    ("web-call", &[KEEP_USES]),
    ("sign", &[KEEP_USES]),
    ("release", &[KEEP_USES]),
];

pub use tilo_actions::remote::VAULT_SECRET_ANSWERS;

/// The step `verb` ([`KEEP_SEALED`], [`REMOVE_SEALED`], [`KEEP_USES`]) of the record the
/// workspace's vault is kept in: what an action that writes the vault declares.
#[must_use]
pub fn vault_step(verb: &str) -> tilo_actions::ActionEffect {
    tilo_actions::ActionEffect::new(tilo_app_id::apps::PASSWORDS, VAULT_KIND, verb)
}

/// The provider the workspace's vault answers under.
pub const PROVIDER: &str = <VaultAction as tilo_resource::Effect>::OPERATION;
/// The resource the vault's own actions are of.
pub const RESOURCE: &str = VAULT_RESOURCE;

/// The vault's actions, each with the parameters its argument carries. A secret typed in
/// travels in [`VaultAction::Add`], [`VaultAction::Keep`] and [`VaultAction::KeepPageLogin`]
/// and is never answered back, so the enum prints no argument.
#[derive(Clone, tilo_resource::Effect)]
#[effect(operation = "tilo.vault")]
pub enum VaultAction {
    /// List vault items
    ///
    /// What is safe to say of every item, as [`KeptItems`].
    #[effect(kind = "read", returns = KeptItems)]
    List,
    /// Keep a vault item
    ///
    /// Keeps an item, or changes the one it names, answering what is safe to say of it as an
    /// [`ItemView`], its reference included. The secret is sealed in the vault and never
    /// answered back.
    #[effect(kind = "undoable", returns = ItemView)]
    Add {
        #[param(
            "The item this changes, by its reference (`vault.item:…`): it keeps its reference and \
             its uses, and each of its secrets, its username, title, note and field values left \
             out stays as it was. Left out for a new item."
        )]
        item: Option<ItemRef>,
        #[param(
            "What it is: `login`, `key`, `code-generator`, `ssh-key`, `note`, `wifi` or `value`."
        )]
        kind: ItemKind,
        #[param(
            "Where it belongs, each a web address such as https://example.com or a server such \
             as imap://mail.example.com:993. A note, a Wi-Fi password or a licence code may \
             belong nowhere.",
            name = "belongsTo"
        )]
        belongs_to: Option<Vec<String>>,
        #[param("What it is called: what a person picks it by.")]
        title: Option<String>,
        #[param("The name it signs in with.")]
        username: Option<String>,
        #[param(
            "For a key, how its service takes it when not as a bearer token: `basic`, \
             `header:<name>` or `query:<name>`.",
            name = "sentIn"
        )]
        sent_in: Option<String>,
        #[param(
            "For a code generator, the hash its codes are made with: `SHA1`, `SHA256` or \
             `SHA512`; `SHA1` when left out."
        )]
        algorithm: Option<String>,
        #[param("For a code generator, how many digits its codes have; 6 when left out.")]
        digits: Option<u8>,
        #[param(
            "For a code generator, how many seconds one of its codes stands; 30 when left out."
        )]
        period: Option<u64>,
        #[param(
            secret,
            "The secret: sealed in the workspace's vault, never answered back. Left out where \
             `generate` draws a login's password."
        )]
        secret: Option<String>,
        #[param(secret, "A note kept with it, sealed as the secret is.")]
        notes: Option<String>,
        #[param(
            secret,
            "The fields kept with it, such as a PIN, each `{label, value}`: each value sealed as \
             its secret is, only the labels ever answered back."
        )]
        fields: Option<Vec<ItemField>>,
        #[param(
            "What the item is tagged with. A secret an app was moved to is tagged with the app \
             and purpose it was kept for, and the app finds it by those."
        )]
        tags: Option<Vec<String>>,
        #[param(
            "For a login without a secret: draw a strong password where the vault is, `length` \
             (12 to 128, 20 when not said) characters, with `capitals`, `digits` and `symbols` \
             unless one is false."
        )]
        generate: Option<Generate>,
    },
    /// Forget a vault item
    ///
    /// Forgets an item, answering the items that remain as [`KeptItems`].
    #[effect(kind = "undoable", returns = KeptItems)]
    Remove {
        #[param("The item, by its reference (`vault.item:…`).")]
        item: ItemRef,
    },
    /// Read the uses of a vault item
    ///
    /// When, where, for whom and what for an item was handed to something that signs in, and
    /// who may use it without its person being asked again, as [`ItemUses`].
    #[effect(kind = "read", returns = ItemUses)]
    Uses {
        #[param("The item, by its reference (`vault.item:…`).")]
        item: ItemRef,
    },
    /// Take back what may always use a vault item
    ///
    /// Takes back the leaves of an item, every one or only those of one agent or app,
    /// answering what the item remembers then, as [`ItemUses`].
    #[effect(kind = "undoable", returns = ItemUses)]
    TakeBack {
        #[param("The item, by its reference (`vault.item:…`).")]
        item: ItemRef,
        #[param("The agent whose leaves to take back.")]
        agent: Option<String>,
        #[param(
            "The app whose leave to read the item to take back. With neither this nor `agent`, \
             every leave of the item goes."
        )]
        app: Option<String>,
    },
    /// Copy a vault item's secret
    ///
    /// Puts one secret of an item on its person's clipboard, which forgets it again after a
    /// while, written down as a use. The secret is answered to nobody: the device that keeps
    /// the vault copies it.
    #[effect(kind = "read")]
    Copy {
        #[param("The item, by its reference (`vault.item:…`).")]
        item: ItemRef,
        #[param(
            "Which part: `secret` (the password, key or value; a note's text) when not said, \
             `notes`, `username` or `field:<label>`."
        )]
        part: Option<String>,
    },
    /// Show a vault item's secret
    ///
    /// Answers one secret of an item to its person, to be shown while they look, as
    /// [`Revealed`], written down as a use. No record of the call keeps it.
    #[effect(kind = "read")]
    Reveal {
        #[param("The item, by its reference (`vault.item:…`).")]
        item: ItemRef,
        #[param(
            "Which part: `secret` (the password, key or value; a note's text) when not said, \
             `notes`, `username` or `field:<label>`."
        )]
        part: Option<String>,
    },
    /// Ask your person for a secret
    ///
    /// Ask your person for a password, a key or another secret. This is the one way to get one:
    /// never ask for one in the conversation, where everyone in it and you read what they
    /// write. Your person types it into the system's protected field on their own device, in
    /// words the system writes; you never see it. The workspace's vault keeps it for the places
    /// in `belongsTo` alone: a site as `https://example.com`, a server as
    /// `imap://mail.example.com:993` or `ssh://git.example.com`. You are told the `reference`
    /// it is kept under (`vault.item:…`), or that they declined: pass the reference where an
    /// action takes the secret. Where nobody watches, the call ends at once and you are told in
    /// your conversation with them; do not make it again.
    #[effect(kind = "outside", audience = "agents", returns = Asked)]
    Ask {
        #[param(flatten)]
        asked: AskForItem,
    },
    /// Fill page field from vault
    ///
    /// Fill a field of a web page from an item your person's vault keeps for that page's site:
    /// a login's password, a code generator's code of this moment, or a kept value. You never
    /// see what the item keeps. When you work with nobody watching, your person is asked first
    /// in the conversation whether you may use the item there: tell them it waits for their
    /// answer and do not make the call again. The item is used when they allow it, and after
    /// their Always without asking.
    #[effect(kind = "outside", audience = "agents")]
    FillPage {
        #[param("The site the page is at, as its window's engine names it.")]
        site: String,
        #[param(
            "The `reference` the vault's `ask` answered with (`vault.item:…`). It names the vault \
             item and is never the secret itself."
        )]
        item: ItemRef,
        #[param(
            "Why you need it, in one sentence your person reads when they are asked whether you \
             may."
        )]
        reason: Option<String>,
    },
    /// Call a service with a key from the vault
    ///
    /// Make a web request to a service that takes a key, with a key your person's vault keeps:
    /// for a service that has no connection of its own. The server adds the key on the way out,
    /// where the item says its service takes it (as a bearer token unless it says otherwise),
    /// and only to the service the item belongs to: `url` must be at that service's web
    /// address, over https. You never see the key, and it is struck from what you are
    /// answered: the status, the headers and the body. What it sends is `body` text,
    /// `bodyParts` (an inline `text` or `json` part, or a `content` part naming content your
    /// workspace keeps by its BLAKE3 hex address, with its filename and content type), or
    /// `bodyContent`, one content reference sent as the whole body. The server reads that
    /// content from the workspace's store: its bytes never pass through you, and you may name
    /// only content the workspace keeps. To get the key, call the vault's `ask` with kind
    /// `key`, belongsTo `["<the service's web address, such as https://api.example.com>"]` and
    /// `sentIn`, which says how the service takes its key when not as a bearer token: `basic`,
    /// `header:<name>` such as `header:X-Api-Key`, or `query:<name>`. Your person types it into
    /// a protected field, and you are told the `reference` the workspace's vault keeps it under.
    /// Pass that reference as `key`. When you work with nobody watching, your person is asked
    /// first in the conversation whether you may make this call, and nothing is sent: tell them
    /// it waits for their answer. Once they allowed it, make the same call again with the same
    /// arguments: it is sent once. Each call is asked about anew.
    #[effect(kind = "outside", audience = "agents")]
    WebCall {
        #[param(flatten)]
        call: WebCall,
    },
    /// Read a code from the vault
    ///
    /// The code of this moment from a code generator's seed, addressed at the item
    /// (`vault.item:<id>#code`), so a grant covers that one account. It is a read: the vault
    /// computes it where the seed is and writes nothing down, so it appears in no change, no
    /// use and no history.
    #[effect(kind = "read")]
    Code {
        #[param("Why whoever asks needs the code, in a sentence its person reads.")]
        reason: Option<String>,
    },
    /// Sign with an ssh key from the vault
    ///
    /// Signs with the ssh key an item keeps, for one of the ssh servers it belongs to,
    /// addressed at the item (`vault.item:<id>#sign`), so a grant covers that one key. The
    /// vault signs where the key is and answers the signature and the key's public line; the
    /// private key never leaves it. Each signature is a use the item remembers.
    #[effect(kind = "outside")]
    Sign {
        #[param(flatten)]
        signing: Sign,
    },
    /// Hand a vault item's secret to an app's process
    ///
    /// Hands one item's secret to a process of an app's own that speaks the item's protocol
    /// itself (a gateway login, voice, an SSH or VNC handshake, a database login), addressed at
    /// the item (`vault.item:<id>#release`), so the grant its person gives on it is a grant on
    /// that item alone. Only the app's own connection is answered, and nothing keeps the
    /// secret: no record, argument, log or history.
    #[effect(kind = "outside")]
    Release,
    /// Find an app's vault item
    ///
    /// Finds the item a moved app's secret was kept as, by the app and purpose tags the app
    /// names, answering its reference as [`Found`]. It is answered only to that app's own
    /// connection, never to a person, a page or an agent.
    #[effect(kind = "read", returns = Found)]
    Find {
        #[param("The app whose tag the item carries.")]
        app: String,
        #[param("The purpose whose tag the item carries.")]
        purpose: String,
    },
    /// Keep an app's own secret
    ///
    /// Keeps a secret an app obtained or made itself, such as an OAuth token or a key it
    /// generated, in the workspace's vault as that app's own, answering the item's reference as
    /// [`Kept`]. It is answered only on the app's own connection: the app is the one that
    /// connection was placed for, never an argument.
    #[effect(kind = "undoable", returns = Kept)]
    Keep {
        #[param(
            "What the app keeps it for, such as `oauth-token`: the app finds it again by this."
        )]
        purpose: String,
        #[param(
            secret,
            "The secret: sealed in the workspace's vault, released back only to the app's own \
             process."
        )]
        value: String,
        #[param(
            "The app's own item whose secret this renews, by its reference; left out, the item \
             the app keeps for the purpose is renewed, or a new one kept."
        )]
        replacing: Option<ItemRef>,
    },
    /// Read this device's key for an app
    ///
    /// The key this device derives for an app and a purpose of its own, as 32 bytes of lowercase
    /// hex: what identifies the app's install on this device to a service that pairs with
    /// devices. The device's daemon derives it from the device's own key, so it is the same for
    /// the same app and purpose on this device, differs on every other device, and is kept
    /// nowhere.
    #[effect(kind = "read")]
    DeviceKey {
        #[param(
            "What the app uses the key for, such as `grayjay-noise`: the same purpose answers the \
             same key on this device."
        )]
        purpose: String,
    },
    /// List a site's logins
    ///
    /// The logins the vault keeps for one site, as [`PageLogins`]: what is safe to say of each,
    /// the name it signs in with included, never its secret. It and the other page login
    /// actions are answered only to a window of an app's own that shows a page, on that
    /// window's own connection.
    #[effect(kind = "read", returns = PageLogins)]
    PageLogins {
        #[param("The site the page is at, as its window's engine says it.")]
        site: String,
    },
    /// Keep a page's login
    ///
    /// Keeps the login a page sent as the app's own, for signing in at that site, answering
    /// what is safe to say of it as a [`PageLogin`].
    #[effect(kind = "undoable", returns = PageLogin)]
    KeepPageLogin {
        #[param("The site the page is at, as its window's engine says it.")]
        site: String,
        #[param("The name its form sent.")]
        username: String,
        #[param(secret, "The password its form sent.")]
        secret: String,
        #[param("The login it replaces, by its reference, if the person chose one.")]
        replacing: Option<ItemRef>,
    },
    /// Forget a page's login
    ///
    /// Forgets a login the app kept, or one it may fill, answering the [`PageLogins`] that
    /// remain for the site.
    #[effect(kind = "undoable", returns = PageLogins)]
    ForgetPageLogin {
        #[param("The site the page is at, as its window's engine says it.")]
        site: String,
        #[param("The login to forget, by its reference.")]
        item: ItemRef,
    },
    /// Fill a page's login
    ///
    /// Hands one login's name and secret to the window whose page is at a site the login
    /// belongs to, for its engine to fill the page's form, addressed at the item
    /// (`vault.item:<id>#page-fill`).
    #[effect(kind = "outside")]
    PageFill {
        #[param("The site the page is at, as its window's engine says it.")]
        site: String,
    },
}

impl std::fmt::Debug for VaultAction {
    /// The action alone: an argument may be a secret.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("VaultAction")
            .field(&self.id())
            .finish()
    }
}

/// Each action's id, as its declaration in [`VaultAction`] names it.
macro_rules! ids {
    ($($id:ident),* $(,)?) => {
        $(
            #[doc = concat!("The id of [`VaultAction::", stringify!($id), "`].")]
            pub const $id: &str = VaultAction::$id.id();
        )*
    };
}

ids! {
    LIST, ADD, REMOVE, USES, TAKE_BACK, COPY, REVEAL, ASK, FILL_PAGE, WEB_CALL, CODE, SIGN,
    RELEASE, FIND, KEEP, DEVICE_KEY, PAGE_LOGINS, KEEP_PAGE_LOGIN, FORGET_PAGE_LOGIN, PAGE_FILL
}

/// The vault's actions a person asks of it, from Passwords or as themselves, in catalogue
/// order: what the workspace's tree offers at `workspace/vault`.
pub const PERSON_ACTIONS: [&str; 7] = [LIST, ADD, REMOVE, USES, TAKE_BACK, COPY, REVEAL];

/// What the app kept a page's login for, as the item names it ([`KEEP_PAGE_LOGIN`]).
pub const SIGN_IN: &str = "sign-in";

/// The declaration of the action `action`, when it is one of [`VaultAction`].
#[must_use]
pub fn declaration(action: &str) -> Option<tilo_resource::EffectDecl> {
    VaultAction::declarations()
        .into_iter()
        .find(|decl| decl.id == action)
}

/// The parameters the action `action` is published with, where it is one of [`VaultAction`]:
/// what the device keeps none of where a parameter is a secret.
#[must_use]
pub fn params_of(action: &str) -> Option<Vec<tilo_actions::ActionParam>> {
    declaration(action).map(|decl| decl.params)
}

/// The call `action` with `arguments`, read as its declaration names them. Nothing the
/// declaration does not name is read past: an argument of its caller's own making, a secret
/// where none is taken above all, refuses the call.
///
/// # Errors
/// The action is none of the vault's, or an argument is unknown or does not read; the reason
/// never quotes the arguments.
pub fn decoded(action: &str, arguments: &Map<String, Value>) -> Result<VaultAction, String> {
    let decl = declaration(action).ok_or_else(|| format!("The vault has no action `{action}`"))?;
    if let Some(unknown) = arguments
        .keys()
        .find(|name| !decl.params.iter().any(|param| param.name == name.as_str()))
    {
        return Err(format!("`{action}` takes no `{unknown}`"));
    }
    VaultAction::decode(action, arguments).map_err(|error| match error {
        tilo_resource::EffectDecodeError::Argument { name, .. } => {
            format!("The `{name}` argument is not readable")
        }
        _ => format!("The arguments of `{action}` are not readable"),
    })
}

/// The target of action `action` of [`RESOURCE`].
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    tilo_actions::remote::vault_target(action)
}

/// What the vault's action `action` writes in the record the workspace keeps it in
/// ([`VAULT_STEPS`]): a read writes nothing.
#[must_use]
pub fn effects(action: &str) -> Vec<tilo_actions::ActionEffect> {
    VAULT_STEPS
        .iter()
        .find(|(id, _)| *id == action)
        .map_or_else(Vec::new, |(_, steps)| {
            steps.iter().map(|step| vault_step(step)).collect()
        })
}

/// The workspace's vault as a view of another language reads it (`vault`): the items it keeps,
/// the records they read as and the actions a view or another crate calls.
#[must_use]
pub fn surface() -> tilo_actions::surface::Surface {
    tilo_resource::surface::reading_surface::<KeptItems, VaultAction>(RESOURCE, "Vault")
}

/// The page login actions, which only a window of an app's own is answered.
pub const PAGE_LOGIN_ACTIONS: [&str; 4] =
    [PAGE_LOGINS, KEEP_PAGE_LOGIN, FORGET_PAGE_LOGIN, PAGE_FILL];

/// The action `target` asks when it is one only an app's own process or window is answered, on
/// the connection the daemon handed it: a [`RELEASE`], [`FIND`] or [`PAGE_FILL`] of whatever it
/// is addressed at, or a web call, a keep, a device key or a page's logins at the vault's own
/// resource. `None` for every other call.
#[must_use]
pub fn asked_by_app(target: &RemoteActionTarget) -> Option<&'static str> {
    let action = target.address.action.as_str();
    let anywhere = [RELEASE, FIND, PAGE_FILL];
    let at_vault = [
        WEB_CALL,
        KEEP,
        DEVICE_KEY,
        PAGE_LOGINS,
        KEEP_PAGE_LOGIN,
        FORGET_PAGE_LOGIN,
    ];
    (target.provider == PROVIDER)
        .then(|| anywhere.into_iter().find(|id| *id == action))
        .flatten()
        .or_else(|| {
            (target.provider == PROVIDER && target.address.resource == RESOURCE)
                .then(|| at_vault.into_iter().find(|id| *id == action))
                .flatten()
        })
}

/// The tag naming the app an adopted item was kept for, as [`FIND`] matches it.
#[must_use]
pub fn app_tag(app: &str) -> String {
    format!("app:{app}")
}

/// The tag naming the purpose an adopted item was kept for, as [`FIND`] matches it.
#[must_use]
pub fn purpose_tag(purpose: &str) -> String {
    format!("purpose:{purpose}")
}

/// The tag naming the item an adopted item was kept as.
#[must_use]
pub fn item_tag(item: &str) -> String {
    format!("item:{item}")
}

/// The word a leave names the use `target` by ([`AlwaysUse::using`]): its provider and its
/// action.
#[must_use]
pub fn use_word(target: &RemoteActionTarget) -> String {
    format!("{}/{}", target.provider, target.address.action)
}

/// Whether `text` could name where an item belongs: a web address, or a server by scheme,
/// host and port. A first check for whoever asks for a secret, before a person is shown the
/// question; the host that keeps the item decides.
#[must_use]
pub fn looks_like_place(text: &str) -> bool {
    let Some((scheme, rest)) = text.split_once("://") else {
        return false;
    };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "https" | "http" | "imap" | "smtp" | "ssh"
    ) && !rest.is_empty()
        && text.len() <= 2_048
        && !text
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

/// What kind of thing an item is.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ItemKind {
    /// A name and a password, for a site or a server.
    Login,
    /// A key a service takes with each request.
    Key,
    /// The seed codes for a second step are made from.
    CodeGenerator,
    /// A private ssh key.
    SshKey,
    /// A plain value that is not a sign-in: a card number, a passport number.
    Value,
    /// A note kept as a secret: its text is the secret, and it belongs nowhere.
    Note,
    /// The password of a Wi-Fi network.
    Wifi,
}

/// What is safe to say of one item, as [`LIST`] and [`ADD`] answer it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemView {
    /// The item's reference, as [`REMOVE`] and every use name it.
    pub reference: ItemRef,
    /// What kind of thing it is.
    pub kind: ItemKind,
    /// Where it belongs: a site's or a service's origin (`https://example.com`), or a server
    /// (`imap://mail.example.com:993`).
    pub belongs_to: Vec<String>,
    /// What it is called, where its entry has a title: what a person picks it by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// When it was added, in seconds since the Unix epoch.
    pub added_at: u64,
    /// When it was last handed to something that signs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<u64>,
    /// The hash a code generator's codes are made with, for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub algorithm: Option<String>,
    /// How many digits a code generator's codes have, for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digits: Option<u8>,
    /// How many seconds one of a code generator's codes stands, for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<u64>,
    /// The tags a person gave it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// The labels of the fields a person added to it, such as `PIN`: never their values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    /// Whether it keeps a note, which is as secret as the item itself and never listed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_notes: bool,
    /// When it last changed, in seconds since the Unix epoch.
    #[serde(default)]
    pub changed_at: u64,
    /// What is wrong with it, for a person to fix: a login's password that is weak or reused,
    /// an item not changed in a year.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub concerns: Vec<Concern>,
}

/// What is wrong with an item, for a person to fix.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Concern {
    /// A login's password shorter than 12 characters, or without a capital, a small letter or
    /// a digit.
    Weak,
    /// A login's password another login of the vault keeps too.
    Reused,
    /// Not changed in more than a year.
    Stale,
}

/// Every item the workspace's vault keeps, oldest first.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeptItems {
    /// What is safe to say of each item.
    pub items: Vec<ItemView>,
}

/// A field a person adds to an item: its label, which is safe to say, and its value, which is
/// as secret as the item.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemField {
    /// What the field is called, such as `PIN`.
    pub label: String,
    /// Its value, sealed as the item's secret is.
    pub value: String,
}

impl std::fmt::Debug for ItemField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ItemField")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// A password the vault draws itself: how long it is, and the kinds of character it takes
/// beside small letters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Generate {
    /// How many characters it has.
    #[serde(default = "Generate::twenty")]
    pub length: u16,
    /// Whether it takes capitals.
    #[serde(default = "Generate::yes")]
    pub capitals: bool,
    /// Whether it takes digits.
    #[serde(default = "Generate::yes")]
    pub digits: bool,
    /// Whether it takes symbols.
    #[serde(default = "Generate::yes")]
    pub symbols: bool,
}

impl Generate {
    /// The shortest password the vault draws.
    pub const SHORTEST: u16 = 12;
    /// The longest password the vault draws.
    pub const LONGEST: u16 = 128;

    const fn twenty() -> u16 {
        20
    }

    const fn yes() -> bool {
        true
    }
}

impl Default for Generate {
    fn default() -> Self {
        Self {
            length: Self::twenty(),
            capitals: true,
            digits: true,
            symbols: true,
        }
    }
}

/// One time an item was handed to something that signs in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemUse {
    /// When, in seconds since the Unix epoch.
    pub at: u64,
    /// The places it was used at; see [`ItemView::belongs_to`].
    pub places: Vec<String>,
    /// The agent it was handed out for, by the name its person knows it under; empty when the
    /// person used it themselves.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    /// What it was used for: the use's own word, such as `tilo.vault/web-call`. Left out where
    /// none was said.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub using: String,
    /// What was done with it there, in one line: a web call's method and path, without the
    /// address's query. Never the secret, never what was sent.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub what: String,
    /// How its person allowed it: `once` where they were asked and allowed that one use,
    /// `always` under a leave they gave ([`AlwaysUse`]). Left out where neither is known.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub allowed: String,
}

/// The uses an item remembers, oldest first: the newest of them.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemUses {
    /// Each time the item was handed to something that signs in.
    pub uses: Vec<ItemUse>,
    /// Who may use the item without its person being asked again: the leaves they gave with
    /// "Always", oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub always: Vec<AlwaysUse>,
}

/// One leave: an agent that may use the item for one use at the places named, without its
/// person being asked again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AlwaysUse {
    /// The agent, by its id: what [`TAKE_BACK`] names. Empty for an app's leave.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent: String,
    /// The app, by its id, that may be handed the item itself ([`RELEASE`]): what
    /// [`TAKE_BACK`] names. Empty for an agent's leave.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub app: String,
    /// The name its person knew it under when they gave the leave.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    /// What it may use the item for: the use's own word ([`use_word`]).
    pub using: String,
    /// The places the leave covers; see [`ItemView::belongs_to`].
    pub places: Vec<String>,
    /// When the person gave it, in seconds since the Unix epoch.
    pub since: u64,
}

/// The parts of an item a person may be handed, as [`COPY`] and [`REVEAL`] name them.
pub mod part {
    /// The part named when none is: the item's own secret.
    pub const SECRET: &str = "secret";
    /// The part that is the note kept with it.
    pub const NOTES: &str = "notes";
    /// The part that is the name it signs in with.
    pub const USERNAME: &str = "username";
    /// What names a field among the parts: `field:<label>`.
    pub const FIELD: &str = "field:";
}

/// The secret [`REVEAL`] answers, for its person to see while they look.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Revealed {
    pub value: String,
}

impl std::fmt::Debug for Revealed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Revealed").finish_non_exhaustive()
    }
}

/// Content the workspace keeps, named by the address of its bytes: what a call sends as a
/// part, or as its whole body, without the bytes passing through the asking app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContentRef {
    /// The app whose records keep the content. The asking app when left out.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub app: String,
    /// The address of the bytes: lowercase BLAKE3 hex.
    pub hash: String,
    /// The filename the part carries, when it carries one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub filename: String,
    /// The bytes' media type (`image/png`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_type: String,
}

/// One part of a call's body: text, JSON, or content its workspace keeps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebCallPart {
    /// The form field's name.
    pub name: String,
    /// The part's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The part's JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    /// Content the workspace keeps, read from its store and sent as the part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<ContentRef>,
}

/// What [`WEB_CALL`] names: the request, and the item that is its key. The key goes where its
/// item says its service takes it, never where the request says.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebCall {
    /// The address to call.
    pub url: String,
    /// GET, HEAD, POST, PUT, PATCH or DELETE; when left out, POST if it sends a body and GET if
    /// not.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub method: String,
    /// Headers to send besides the key, by name. Not the one the key rides in, and none that
    /// signs the request in.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
    /// What to send, as text. Not beside `bodyParts` or `bodyContent`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    /// What to send, as the parts of a multipart body: each names its field and has `text`,
    /// `json`, or `content`, content the workspace keeps by its BLAKE3 hex address with a
    /// `filename` and `contentType`. The vault reads the bytes from the workspace's store, so
    /// they never pass through you.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body_parts: Vec<WebCallPart>,
    /// Content the workspace keeps, by its BLAKE3 hex address with a `filename` and
    /// `contentType`, sent as the whole body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_content: Option<ContentRef>,
    /// The `reference` the vault's `ask` answered with (`vault.item:…`). It names the vault
    /// item and is never the secret itself.
    pub key: ItemRef,
    /// Why you need it, in one sentence your person reads when they are asked whether you may.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// What [`SIGN`] names: the ssh server the signature is for, what is signed, and how.
///
/// With a `namespace` the answer is an armored SSHSIG signature of `data`, what
/// `ssh-keygen -Y sign -n <namespace>` makes (git signs commits in the namespace `git`).
/// Without one, `data` is an ssh sign-in request for the item's key at that server, and the
/// answer is the signature blob ssh public-key authentication sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Sign {
    /// The ssh server it signs for, one the item belongs to: `ssh://git.example.com`.
    pub place: String,
    /// What is signed, in base64.
    pub data: String,
    /// The SSHSIG namespace; empty for an ssh sign-in.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub namespace: String,
    /// Why whoever asks says it needs the signature; empty when it gave none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// What [`RELEASE`] answers: the item's secret, for the process that asked alone. It prints
/// nothing of the secret.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Released {
    pub secret: String,
}

impl std::fmt::Debug for Released {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Released").finish_non_exhaustive()
    }
}

/// What [`FIND`] answers: the item's reference, or `None` when the vault keeps no item tagged
/// with that app and purpose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Found {
    /// The item's reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<ItemRef>,
}

/// What [`KEEP`] answers: the reference of the item the secret is kept as.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Kept {
    /// The item's reference.
    pub reference: ItemRef,
}

/// What [`DEVICE_KEY`] answers: 32 bytes of key, as lowercase hex. It prints nothing of the key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceKey {
    pub key: String,
}

impl std::fmt::Debug for DeviceKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("DeviceKey").finish_non_exhaustive()
    }
}

/// What is safe to say of one login a page is offered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageLogin {
    /// The item's reference, which filling, replacing or forgetting it names.
    pub reference: ItemRef,
    /// What it is called.
    pub title: String,
    /// The name it signs in with.
    pub username: String,
    /// The sites it belongs to.
    pub belongs_to: Vec<String>,
    /// Whether the asking app kept it itself, so filling it asks nobody.
    pub own: bool,
}

/// The logins the vault keeps for one site, oldest first.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageLogins {
    /// What is safe to say of each login.
    pub logins: Vec<PageLogin>,
}

/// What [`PAGE_FILL`] answers: the login's name and secret, for the window that asked alone. It
/// prints nothing of the secret.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageFilled {
    pub username: String,
    pub secret: String,
}

impl std::fmt::Debug for PageFilled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("PageFilled").finish_non_exhaustive()
    }
}

/// The most places one asked-for secret belongs to.
pub const MAX_ASKED_PLACES: usize = 4;
/// The longest user name and `sentIn` an ask names, in bytes.
const MAX_ASKED_NAME_BYTES: usize = 320;

/// What [`ASK`] asks the person for: what kind of secret, where it belongs and the name it signs
/// in with. It names no secret, and nothing in it is worded by the agent: the person reads the
/// system's words for it ([`Self::words`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AskForItem {
    /// What it is: `login` (a password), `key`, `code-generator`, `ssh-key` or `value`.
    pub kind: ItemKind,
    /// Where it belongs: each a web address or a server, at most four.
    pub belongs_to: Vec<String>,
    /// The name it signs in with, when you know it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    /// For a key: how its service takes it when not as a bearer token: `basic`,
    /// `header:<name>` or `query:<name>`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sent_in: String,
}

impl AskForItem {
    /// The ask as it is put: places that each could name one, a name on one line.
    ///
    /// # Errors
    /// What makes it no ask to put, in words that never quote it.
    pub fn checked(mut self) -> Result<Self, String> {
        if self.belongs_to.is_empty() || self.belongs_to.len() > MAX_ASKED_PLACES {
            return Err(format!(
                "belongsTo: 1 to {MAX_ASKED_PLACES} places, each a web address such as \
                 https://example.com or a server such as imap://mail.example.com:993"
            ));
        }
        for place in &mut self.belongs_to {
            *place = place.trim().to_owned();
            if !looks_like_place(place) {
                return Err(
                    "belongsTo: each place is a web address such as https://example.com or a \
                     server such as imap://mail.example.com:993 or ssh://git.example.com"
                        .to_owned(),
                );
            }
        }
        self.username = self.username.trim().to_owned();
        self.sent_in = self.sent_in.trim().to_owned();
        for (name, said) in [("username", &self.username), ("sentIn", &self.sent_in)] {
            if said.len() > MAX_ASKED_NAME_BYTES || said.chars().any(char::is_control) {
                return Err(format!("{name}: a few words on one line"));
            }
        }
        if !self.sent_in.is_empty() && self.kind != ItemKind::Key {
            return Err("sentIn: only a key says how it is sent".to_owned());
        }
        Ok(self)
    }

    /// What the person reads where they are asked, in the system's words: what is asked for,
    /// where it goes, and that `agent` never sees it.
    #[must_use]
    pub fn words(&self, agent: &str) -> String {
        let what = match self.kind {
            ItemKind::Login => "the password",
            ItemKind::Key => "the key",
            ItemKind::CodeGenerator => "the setup key of the code generator",
            ItemKind::SshKey => "the ssh key",
            ItemKind::Value => "the value",
            ItemKind::Note => "the note",
            ItemKind::Wifi => "the Wi-Fi password",
        };
        let places = self.belongs_to.join(" and ");
        let of = if self.username.is_empty() {
            String::new()
        } else {
            format!(" of {}", self.username)
        };
        let alone = if self.belongs_to.len() == 1 {
            "that place alone"
        } else {
            "those places alone"
        };
        format!(
            "{agent} asks for {what}{of} at {places}. {agent} never sees what you type: your \
             vault keeps it for {alone}."
        )
    }
}

/// What [`ASK`] is answered with: the item the workspace's vault now keeps, or none where its
/// person declined.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Asked {
    /// The item's reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<ItemRef>,
}

/// What a turn names when it asks for a field of the page in its workspace's Browser window to
/// be filled ([`FILL_PAGE`], as [`fill_row`] offers it): the field, the item and why. The site is
/// not the turn's to name: the window's engine says where its page is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FillField {
    /// The field, by its reference in the page's outline.
    pub element: String,
    /// The item, by its reference.
    pub item: ItemRef,
    /// Why whoever asks says it needs the item; empty when it gave none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// What every use by reference says of the reference it takes.
const REFERENCE: &str = "The `reference` the vault's `ask` answered with (`vault.item:…`). It \
     names the vault item and is never the secret itself.";
/// What every use by reference says of the reason it may carry.
const REASON: &str = "Why you need it, in one sentence your person reads when they are asked \
     whether you may.";
/// What every use by reference says of being asked first.
const ASKED_FIRST: &str = "When you work with nobody watching, your person is asked first in the \
     conversation whether you may use the item there: tell them it waits for their answer and \
     do not make the call again. The item is used when they allow it, and after their Always \
     without asking.";

/// One use by reference, as a catalogue offers it to an agent: it reaches outside the
/// workspace, writes down the use in the vault, and takes an item's address, which a person
/// never picks from a list.
#[must_use]
pub fn use_row(
    (provider, resource, label): (&str, &str, &str),
    (action, action_label): (&str, &str),
    description: String,
    params: Vec<tilo_actions::ActionParam>,
) -> CatalogueRow {
    let mut contract = tilo_actions::ActionContract {
        kind: tilo_actions::ActionKind::Outside,
        effects: if provider == PROVIDER {
            effects(action)
        } else {
            vec![vault_step(tilo_actions::remote::KEEP_USES)]
        },
        ..tilo_actions::ActionContract::default()
    };
    contract.set_audience(tilo_actions::contract::Audience::Agents);
    CatalogueRow::for_action(
        provider,
        provider,
        tilo_actions::ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(resource, action),
            resource_kind: resource.to_owned(),
            resource_label: label.to_owned(),
            action_label: action_label.to_owned(),
            keywords: Vec::new(),
            icon: None,
            description: Some(description),
            locus: tilo_actions::ExecutionLocus::ServerPreferred,
            kind: tilo_actions::OperationKind::Effect,
            contract,
            input: tilo_actions::InputSpec::None,
            params,
            view: None,
        },
    )
}

/// The vault's own action `action` as a catalogue offers it to an agent, as it is declared.
#[expect(
    clippy::expect_used,
    reason = "`action` is one of the ids `VaultAction` itself declares (`ids!`)"
)]
fn declared_row(action: &str) -> CatalogueRow {
    let decl = declaration(action).expect("the vault declares each of its own ids");
    use_row(
        (PROVIDER, RESOURCE, "Vault"),
        (decl.id, &decl.label.read()),
        decl.description.to_owned(),
        decl.params,
    )
}

/// The actions of other providers that use an item by reference, as a catalogue offers them to
/// an agent acting for the person: adding a mail account whose password the vault keeps, and
/// keeping a provider's key from it. Each takes the item's reference where the secret would
/// stand, and says in its description how the reference is come by, so a model that lists the
/// action reads how to ask and needs no instruction of its own.
#[must_use]
pub fn use_rows() -> Vec<CatalogueRow> {
    use tilo_actions::remote::item_param;
    use tilo_actions::{ActionParam, ParamType};
    vec![
        use_row(
            (ACCOUNTS_PROVIDER, "accounts", "Mail accounts"),
            ("add-mail-with-item", "Add mail account from vault"),
            format!(
                "Add a mail account for your person, signing in with a password their vault \
                 keeps. You never see the password. To get it, call the vault's `ask` with \
                 kind `login`, belongsTo `[\"imap://<incomingHost>:<incomingPort>\", \
                 \"smtp://<outgoingHost>:<outgoingPort>\"]` and the username: your person \
                 types it into a protected field, and you are told the `reference` their \
                 vault keeps it under. Pass that reference as `password`. The item must be a \
                 login that belongs to both servers, and it is sent nowhere else. \
                 {ASKED_FIRST}"
            ),
            vec![
                ActionParam::required("address", ParamType::Text, "The account's mail address."),
                ActionParam::optional(
                    "displayName",
                    ParamType::Text,
                    "The name mail from it carries.",
                ),
                ActionParam::optional(
                    "username",
                    ParamType::Text,
                    "The name it signs in with, when that is neither the item's own nor the \
                     address.",
                ),
                ActionParam::required(
                    "incomingHost",
                    ParamType::Text,
                    "The server mail is read from (IMAP).",
                ),
                ActionParam::optional(
                    "incomingPort",
                    ParamType::Integer,
                    "Its port; 993 when left out.",
                ),
                ActionParam::required(
                    "outgoingHost",
                    ParamType::Text,
                    "The server mail is sent through (SMTP).",
                ),
                ActionParam::optional(
                    "outgoingPort",
                    ParamType::Integer,
                    "Its port; 465 when left out.",
                ),
                item_param("password", REFERENCE),
                ActionParam::optional("reason", ParamType::Text, REASON),
            ],
        ),
        use_row(
            (MODELS_PROVIDER, "models", "Model providers"),
            ("set-key-from-item", "Keep provider key from vault"),
            format!(
                "Keep the API key of one of your person's model providers from a key their \
                 vault keeps. You never see the key. To get it, call the vault's `ask` with \
                 kind `key` and belongsTo `[\"<the web address of the provider's endpoint, \
                 such as https://api.example.com>\"]`: your person types it into a protected \
                 field, and you are told the `reference` the workspace's vault keeps it under. \
                 Pass that reference as `key`. The item must be a key that belongs to the \
                 provider's endpoint, and it is kept for no other. {ASKED_FIRST}"
            ),
            vec![
                ActionParam::required(
                    "provider",
                    ParamType::Text,
                    "The provider's id, as your person's model providers list it.",
                ),
                item_param("key", REFERENCE),
                ActionParam::optional("reason", ParamType::Text, REASON),
            ],
        ),
    ]
}

/// [`ASK`] as a catalogue offers it to an agent acting for the person.
#[must_use]
pub fn ask_row() -> CatalogueRow {
    declared_row(ASK)
}

/// The actions that send what an item keeps out with a request of the agent's own making, as a
/// catalogue offers them to an agent acting for the person: a web call with a key. They stand
/// apart from [`use_rows`] because no "Always" stands for them: what such a request does is the
/// agent's to choose each time, so its person is shown each one.
#[must_use]
pub fn call_rows() -> Vec<CatalogueRow> {
    vec![declared_row(WEB_CALL)]
}

/// [`FILL_PAGE`] as a person is shown it when they are asked first: the site, and the item
/// withheld.
#[must_use]
pub fn page_use() -> Option<tilo_actions::ActionDescriptor> {
    declared_row(FILL_PAGE).action
}

/// [`FILL_PAGE`] as a turn is offered it by whoever can hand the fill to its workspace's
/// Browser window: the turn names the field and the item ([`FillField`]), and the window names
/// the site.
#[must_use]
pub fn fill_row() -> CatalogueRow {
    use tilo_actions::remote::item_param;
    use tilo_actions::{ActionParam, ParamType};
    use_row(
        (PROVIDER, RESOURCE, "Vault"),
        (FILL_PAGE, "Fill page field from vault"),
        format!(
            "Fill a field of the page in this workspace's Browser window from an item your \
             person's vault keeps for that page's site: a login's password, a code generator's \
             code of this moment, or a kept value. You name the field as the page's outline \
             names it and the item by its reference, and you never see what the item keeps. \
             {ASKED_FIRST}"
        ),
        vec![
            ActionParam::required(
                "element",
                ParamType::Text,
                "The field, by its reference in the page's outline.",
            ),
            item_param("item", REFERENCE),
            ActionParam::optional("reason", ParamType::Text, REASON),
        ],
    )
}

/// The use by reference `target` names, as [`use_rows`] offers it, or [`FILL_PAGE`] as a
/// person is shown it; `None` for every other action.
#[must_use]
pub fn use_of(target: &RemoteActionTarget) -> Option<tilo_actions::ActionDescriptor> {
    if *target == self::target(FILL_PAGE) {
        return page_use();
    }
    use_rows().into_iter().find_map(|row| {
        let action = row.action?;
        (row.provider == target.provider && action.address == target.address).then_some(action)
    })
}

/// The use by reference `target` names, as [`use_rows`] or [`call_rows`] offers it; `None` for
/// every other action.
#[must_use]
pub fn described_use(target: &RemoteActionTarget) -> Option<tilo_actions::ActionDescriptor> {
    use_of(target).or_else(|| {
        call_rows().into_iter().find_map(|row| {
            let action = row.action?;
            (row.provider == target.provider && action.address == target.address).then_some(action)
        })
    })
}

#[cfg(test)]
mod tests;

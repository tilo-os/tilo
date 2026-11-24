//! What a person keeps to sign in with, kept where their workspace runs.
//!
//! A login, a key, a code generator's seed, an ssh key or a plain value such
//! as a card number is an item of the person's own vault on the server that
//! runs their workspace, bound to where it belongs: a site, a service, a
//! server. Only the person's own device
//! adds one, lists them or removes one; an agent's request is refused. A
//! secret travels in with [`ADD`] and never back, and so do a note and the
//! values of the fields kept with it: a listing says an item's title, kind,
//! tags, its fields' labels, whether it keeps a note, where it belongs, when
//! it was added, changed and last used, and whether a login's password is
//! weak or reused, and nothing else.
//!
//! Passwords is the person's window on the same vault: it lists, adds and
//! removes items, reads their uses and takes leaves back with the calls
//! above, as its own person on their own device. A secret leaves the vault
//! for that person alone, and only one at a time: [`COPY`] puts it on their
//! clipboard, which forgets it again, and [`REVEAL`] answers it to be shown.
//! Each is written down as a use, and no record of the call keeps it.
//!
//! What an agent holds is a reference ([`ItemRef`]): the address of an item,
//! which is not its secret. An action that needs a secret takes one as a
//! parameter ([`item_param`]). Whatever signs in resolves it on the server,
//! where the secret is used, and only at a place the item belongs to; each
//! use is remembered with when, where, for whom and what for ([`USES`]),
//! never with what was handed.
//!
//! A use made with nobody watching, by an agent that works for the person on
//! their server, asks the person first, in the conversation the agent works in.
//! Their "Always" is a leave kept with the item: for that agent,
//! that use and those places. [`USES`] lists the leaves beside the uses, and
//! [`TAKE_BACK`] takes them back.

use serde::{Deserialize, Serialize};

use super::{ActionAddress, CatalogueRow, RemoteActionTarget, model_providers, person_accounts};

/// The provider that answers the person's vault under.
pub const PROVIDER: &str = "tilo.vault";
/// The resource the items are actions of.
pub const RESOURCE: &str = "vault";
/// What is safe to say of every item, as a [`KeptItems`].
pub const LIST: &str = "list";
/// Keep an item ([`AddItem`]), answering what is safe to say of it as an
/// [`ItemView`], its reference included.
pub const ADD: &str = "add";
/// Forget an item ([`RemoveItem`]), answering the items that remain as a
/// [`KeptItems`].
pub const REMOVE: &str = "remove";
/// When, where, for whom and what for an item was handed to something that
/// signs in ([`UsesOf`]), as an [`ItemUses`].
pub const USES: &str = "uses";
/// Take back the leaves of an item ([`TakeBack`]): who may use it
/// without its person being asked again. Answers what the item remembers
/// then, as an [`ItemUses`].
pub const TAKE_BACK: &str = "take-back";

/// Put one secret of an item ([`ShowItem`]) on its person's clipboard, which forgets it again
/// after a while, written down as a use. The secret is answered to nobody: the device that
/// holds the vault copies it.
pub const COPY: &str = "copy";
/// Answer one secret of an item ([`ShowItem`]) to its person, to be shown while they look, as
/// [`Revealed`], written down as a use. No record of the call keeps it
/// ([`answers_secret`]).
pub const REVEAL: &str = "reveal";

/// The use that fills a field of a web page from an item, by reference
/// ([`FillPage`]): the action a leave names it by ([`use_word`]) and a
/// narrowed agent's rule names. No catalogue offers it by itself
/// ([`use_rows`]): whoever holds the page's window takes it for the turn
/// that asked, with the site the window's engine says the page is at.
pub const FILL_PAGE: &str = "fill-page";

/// Ask the person for a secret ([`AskForItem`]): the one way an agent comes by one. The person
/// types it into the system's protected password field on a device of their own, in the
/// system's words ([`AskForItem::words`]), and the workspace's vault keeps it for the places asked for
/// alone. Whoever asked is answered the item's reference ([`Asked`]), never the secret. An
/// agent's question is answered only with its choices, and what a person says in a conversation
/// is read by everyone in it and by the agent's model, so neither is a way to one.
pub const ASK: &str = "ask";

/// The record kind the workspace keeps its vault in, in Passwords: where every action of the
/// vault that writes takes its step ([`effects`]).
pub const VAULT_KIND: &str = tilo_action_policy::roles::WORKSPACE_VAULT_KIND;
/// The vault record's step that keeps an item, a new one or an edit of one.
pub const KEEP_SEALED: &str = "keep-sealed";
/// The vault record's step that forgets an item.
pub const REMOVE_SEALED: &str = "remove-sealed";
/// The vault record's step that writes down an item's uses.
pub const KEEP_USES: &str = "keep-uses";

/// What the vault's action `action` writes: the step the workspace's host takes in the record it
/// keeps the vault in when it answers the call ([`VAULT_KIND`] in Passwords). It is the action's
/// own declaration, so a device reading the step accepts it as the call's, and an agent listing
/// the action reads what it writes. A read writes nothing.
#[must_use]
pub fn effects(action: &str) -> Vec<crate::ActionEffect> {
    match action {
        ADD | ASK => vec![vault_step(KEEP_SEALED)],
        REMOVE => vec![vault_step(REMOVE_SEALED)],
        COPY | REVEAL | FILL_PAGE | WEB_CALL | SIGN | RELEASE => vec![vault_step(KEEP_USES)],
        _ => Vec::new(),
    }
}

/// The step `verb` in the record the workspace keeps its vault in.
/// The step `verb` ([`KEEP_SEALED`], [`REMOVE_SEALED`], [`KEEP_USES`]) of the record the
/// workspace's vault is kept in: what an action that writes the vault declares.
#[must_use]
pub fn vault_step(verb: &str) -> crate::ActionEffect {
    crate::ActionEffect::new(tilo_app_id::apps::PASSWORDS, VAULT_KIND, verb)
}

/// What the action `target` names writes in the workspace's vault, as it declares it: the
/// vault's own actions ([`effects`]), whether asked of [`PROVIDER`] or of the workspace's tree
/// (`workspace/vault`), and a use by reference of another app, which writes down the use
/// ([`described_use`]).
#[must_use]
pub fn declared_effects(target: &RemoteActionTarget) -> Vec<crate::ActionEffect> {
    let in_workspace_tree = target.provider == super::WORKSPACE_GOVERNANCE_PROVIDER
        && target
            .address
            .resource
            .strip_prefix(super::WORKSPACE_GOVERNANCE_KIND)
            .and_then(|rest| rest.strip_prefix(':'))
            .and_then(|rest| rest.strip_suffix(super::WORKSPACE_VAULT))
            .and_then(|workspace| workspace.strip_suffix('/'))
            .is_some_and(|workspace| !workspace.contains('/'));
    if target.provider == PROVIDER || in_workspace_tree {
        return effects(&target.address.action);
    }
    described_use(target).map_or_else(Vec::new, |action| action.effects)
}

/// The kind of resource a reference is the address of: what a parameter that
/// takes a secret by reference names as its resource kind.
pub const ITEM_KIND: &str = "vault.item";

/// The code of this moment from a code generator's seed ([`VaultItems::code_for`]). It is a
/// read: the vault computes it where the seed is and writes nothing down, so it appears in no
/// change, no use and no history. The person, their agents, Chat and a script ask it alike.
pub const CODE: &str = "code";

/// Find the item a moved app's secret was kept as ([`FindItem`]), by the app and purpose tags
/// the app names, answering its reference as [`Found`]. It is answered only to that app's held
/// connection ([`RELEASE`]), never to a person, a page or an agent.
pub const FIND: &str = "find";

/// The resource an item's own actions are addressed at: `vault.item:<id>`.
#[must_use]
pub fn item_resource(item: &ItemRef) -> String {
    format!("{ITEM_KIND}:{}", item.id())
}

/// The target of [`CODE`] on `item`: `vault.item:<id>#code`, whose resource is the item, so a
/// grant covers that one account and no other.
#[must_use]
pub fn code_target(item: &ItemRef) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(item_resource(item), CODE),
    }
}

/// Sign with the ssh key an item keeps, for one of the ssh servers it belongs to ([`Sign`]),
/// addressed at the item: `vault.item:<id>#sign` ([`sign_target`]), so a grant covers that one
/// key. The vault signs where the key is and answers the signature and the key's public line;
/// the private key never leaves it. Each signature is a use the item remembers.
pub const SIGN: &str = "sign";

/// The target of [`SIGN`] on `item`: `vault.item:<id>#sign`.
#[must_use]
pub fn sign_target(item: &ItemRef) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(item_resource(item), SIGN),
    }
}

/// The item a [`SIGN`] at `target` is addressed at; `None` for any other call, or a signature
/// asked of something that is no item.
#[must_use]
pub fn signing_item(target: &RemoteActionTarget) -> Option<ItemRef> {
    (target.provider == PROVIDER && target.address.action == SIGN)
        .then(|| ItemRef::parse(&target.address.resource))
        .flatten()
}

/// What [`SIGN`] names: the ssh server the signature is for, what is signed, and how.
///
/// With a `namespace` the answer is an armored SSHSIG signature of `data`, what
/// `ssh-keygen -Y sign -n <namespace>` makes (git signs commits in the namespace `git`).
/// Without one, `data` is an ssh sign-in request for the item's key at that server, and the
/// answer is the signature blob ssh public-key authentication sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

/// Whether `text` could name where an item belongs: a web address, or a
/// server by scheme, host and port. A first check for whoever asks for a
/// secret, before a person is shown the question; the host that keeps the
/// item decides.
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

/// The target of action `action` of [`RESOURCE`].
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// What kind of thing an item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// A plain value that is not a sign-in: a card number, a passport
    /// number.
    Value,
    /// A note kept as a secret: its text is the secret, and it belongs nowhere.
    Note,
    /// The password of a Wi-Fi network.
    Wifi,
}

impl ItemKind {
    /// The word a person reads it by ("login", "code-generator").
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Key => "key",
            Self::CodeGenerator => "code-generator",
            Self::SshKey => "ssh-key",
            Self::Value => "value",
            Self::Note => "note",
            Self::Wifi => "wifi",
        }
    }
}

/// A reference to one item of a person's vault: its address
/// (`vault.item:<id>`), which names it and is not its secret.
///
/// An agent may hold one, pass it as an argument and keep it in a document.
/// It opens nothing by itself: the host resolves it only for the person
/// whose vault holds the item, and only at a place the item belongs to. The
/// id is random, so a reference cannot be guessed from another.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ItemRef {
    id: String,
}

impl ItemRef {
    /// The reference to the entry of the workspace's vault `id` names, when `id` is an
    /// entry's id: 32 lowercase hexadecimal digits, or an older entry's, the same spelled as a
    /// UUID with its four hyphens.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        let hex = |part: &str| {
            part.bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        let entry = match id.len() {
            32 => hex(id),
            36 => {
                let parts: Vec<&str> = id.split('-').collect();
                parts.iter().map(|part| part.len()).eq([8, 4, 4, 4, 12])
                    && parts.iter().all(|part| hex(part))
            }
            _ => false,
        };
        entry.then(|| Self { id: id.to_owned() })
    }

    /// The reference `address` spells, when it is one.
    #[must_use]
    pub fn parse(address: &str) -> Option<Self> {
        let id = address.strip_prefix(ITEM_KIND)?.strip_prefix(':')?;
        Self::from_id(id)
    }

    /// The id of the item it names.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl std::fmt::Display for ItemRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{ITEM_KIND}:{}", self.id)
    }
}

impl TryFrom<String> for ItemRef {
    type Error = &'static str;

    fn try_from(address: String) -> Result<Self, Self::Error> {
        Self::parse(&address).ok_or("a reference to a vault item reads vault.item:<id>")
    }
}

impl From<ItemRef> for String {
    fn from(reference: ItemRef) -> Self {
        reference.to_string()
    }
}

/// What is safe to say of one item, as [`LIST`] and [`ADD`] answer it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemView {
    /// The item's reference, as [`REMOVE`] and every use name it.
    pub reference: ItemRef,
    pub kind: ItemKind,
    /// Where it belongs: a site's or a service's origin
    /// (`https://example.com`), or a server (`imap://mail.example.com:993`).
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
    /// What is wrong with it, for a person to fix: a login's password that is weak or
    /// reused, an item not changed in a year.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub concerns: Vec<Concern>,
}

/// What is wrong with an item, for a person to fix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// Every item the person's vault keeps, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeptItems {
    pub items: Vec<ItemView>,
}

/// An item to keep. The secret travels here and nowhere back: the host
/// seals it in the person's vault and answers only an [`ItemView`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddItem {
    /// The item this changes in place, keeping its reference and its uses: each of its
    /// secrets, its username, title, note and field values left empty here stays as it was.
    /// `None` for a new item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemRef>,
    pub kind: ItemKind,
    /// Where it belongs, each a web address or a server; see
    /// [`ItemView::belongs_to`]. A page's address names its site. A login a person keeps before
    /// they know where it signs in belongs nowhere yet.
    #[serde(default)]
    pub belongs_to: Vec<String>,
    /// The name it signs in with, when it has one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    /// Where a key rides in a request to its service, when not as a bearer
    /// token: `basic`, `header:<name>`, `header:<name>:<word before the
    /// key>` or `query:<name>`. Only a key says so.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sent_in: String,
    /// The hash a code generator's codes are made with: `SHA1`, `SHA256` or `SHA512`; `SHA1`
    /// where it is left out. Only a code generator says so.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub algorithm: String,
    /// How many digits a code generator's codes have; 6 where it is left out.
    #[serde(default)]
    pub digits: Option<u8>,
    /// How many seconds one of a code generator's codes stands; 30 where it is left out.
    #[serde(default)]
    pub period: Option<u64>,
    /// What it is called: what a person picks it by.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// A note kept with it: sealed as its secret is, never answered back.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// The fields a person adds to it, such as a PIN: each value sealed as its secret is, and
    /// only the labels ever answered back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<ItemField>,
    /// The tags a person gives it, and the tags a keeper that moved an item here names its app,
    /// its purpose and the item it was kept as, so the app finds it by its own tags
    /// (`vault#find`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Draw a strong password for it where the vault is, in place of a secret sent here: so
    /// nobody types it, sees it or sends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generate: Option<Generate>,
    /// The secret. Left out only where [`Self::generate`] draws one.
    #[serde(default)]
    pub secret: String,
}

impl std::fmt::Debug for AddItem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AddItem")
            .field("kind", &self.kind)
            .field("belongs_to", &self.belongs_to)
            .field("tags", &self.tags)
            .finish_non_exhaustive()
    }
}

/// A field a person adds to an item: its label, which is safe to say, and its value, which is
/// as secret as the item.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemField {
    pub label: String,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Generate {
    #[serde(default = "Generate::twenty")]
    pub length: u16,
    #[serde(default = "Generate::yes")]
    pub capitals: bool,
    #[serde(default = "Generate::yes")]
    pub digits: bool,
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

/// A parameter that takes a secret by reference: the address of an item of
/// the vault of the person the caller acts for ([`ItemRef`]), never the
/// secret's text.
///
/// It is a resource parameter of kind [`ITEM_KIND`], so every surface that
/// lists, checks or records arguments carries it as it carries any record's
/// address, and a draft shows it withheld, as it shows a secret.
#[must_use]
pub fn item_param(name: &'static str, description: &'static str) -> crate::ActionParam {
    crate::ActionParam::required(name, crate::ParamType::Resource, description)
        .of_resource_kind(ITEM_KIND)
}

/// Whether `param` takes a secret by reference ([`item_param`]).
#[must_use]
pub fn takes_item(param: &crate::ActionParam) -> bool {
    param.ty == crate::ParamType::Resource && param.resource_kind() == Some(ITEM_KIND)
}

impl ItemRef {
    /// The reference bound to the parameter `name` of `args`, when what is
    /// bound there is one.
    #[must_use]
    pub fn bound(args: &crate::ActionArgs, name: &str) -> Option<Self> {
        match args.get(name)? {
            crate::ArgValue::Resource(address) | crate::ArgValue::Text(address) => {
                Self::parse(address)
            }
            _ => None,
        }
    }
}

/// The item whose uses to read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsesOf {
    pub item: ItemRef,
}

/// One time an item was handed to something that signs in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemUse {
    /// When, in seconds since the Unix epoch.
    pub at: u64,
    /// The places it was used at; see [`ItemView::belongs_to`].
    pub places: Vec<String>,
    /// The agent it was handed out for, by the name its person knows it
    /// under; empty when the person used it themselves.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    /// What it was used for: the use's own word, such as
    /// `tilo.vault/web-call`. Left out where none was said.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub using: String,
    /// What was done with it there, in one line: a web call's method and
    /// path, without the address's query. Never the secret, never what was
    /// sent.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub what: String,
    /// How its person allowed it: `once` where they were asked and allowed
    /// that one use, `always` under a leave they gave ([`AlwaysUse`]). Left
    /// out where neither is known.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub allowed: String,
}

/// The uses an item remembers, oldest first: the newest of them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemUses {
    pub uses: Vec<ItemUse>,
    /// Who may use the item without its person being asked again: the
    /// leaves they gave with "Always", oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub always: Vec<AlwaysUse>,
}

/// One leave: an agent that may use the item for one use at the
/// places named, without its person being asked again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AlwaysUse {
    /// The agent, by its id: what [`TakeBack::agent`] names. Empty for an
    /// app's leave.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent: String,
    /// The app, by its id, that may be handed the item itself ([`RELEASE`]):
    /// what [`TakeBack::app`] names. Empty for an agent's leave.
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

/// The item to forget.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveItem {
    pub item: ItemRef,
}

/// The secret of an item to copy or reveal: the item's own when no part is named.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShowItem {
    pub item: ItemRef,
    /// Which part: `secret` (the password, key or value; a note's text), `notes`,
    /// `field:<label>` for a field the person added, or `username`, the name it signs in with,
    /// which no listing says either.
    #[serde(default = "ShowItem::secret_part")]
    pub part: String,
}

impl ShowItem {
    /// The part named when none is: the item's own secret.
    pub const SECRET: &str = "secret";
    /// The part that is the note kept with it.
    pub const NOTES: &str = "notes";
    /// The part that is the name it signs in with.
    pub const USERNAME: &str = "username";
    /// What names a field among the parts: `field:<label>`.
    pub const FIELD: &str = "field:";

    fn secret_part() -> String {
        Self::SECRET.to_owned()
    }
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

/// Whether `request` answers a secret, so that whatever records it keeps neither its answer nor
/// its arguments: the vault's [`REVEAL`], asked of its provider or of the vault in its
/// workspace's tree, and a call of the action registry that names it by address.
#[must_use]
pub fn answers_secret(request: &super::InvokeRequest) -> bool {
    let target = &request.target;
    (target.address.action == REVEAL && is_vault(target)) || calls_by_address(request, REVEAL)
}

/// Whether `request` is a call of the action registry (`tilo.actions#call`) that names the
/// vault's `action` by its address (`tilo.vault/vault#<action>`, or the vault in a workspace's
/// tree): what Passwords' code makes of the vault, whose record follows the vault's own.
#[must_use]
pub fn calls_by_address(request: &super::InvokeRequest, action: &str) -> bool {
    let target = &request.target;
    target.address.action == "call"
        && target.provider == super::ACTIONS_PROVIDER
        && match &request.argument {
            super::InvokeArgument::Json(arguments) => arguments
                .get("address")
                .and_then(serde_json::Value::as_str)
                .and_then(|address| address.strip_suffix(action))
                .and_then(|rest| rest.strip_suffix('#'))
                .is_some_and(|rest| rest.ends_with(super::WORKSPACE_VAULT)),
            super::InvokeArgument::None | super::InvokeArgument::Text(_) => false,
        }
}

/// Whether `target` is asked of the vault: its provider, or the vault in its workspace's own
/// tree.
fn is_vault(target: &RemoteActionTarget) -> bool {
    target.provider == PROVIDER
        || target.address.resource == RESOURCE
        || target
            .address
            .resource
            .strip_suffix(super::WORKSPACE_VAULT)
            .is_some_and(|rest| rest.ends_with('/'))
}

/// The item whose leaves to take back: every one, or only those
/// of one agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TakeBack {
    pub item: ItemRef,
    /// The agent whose leaves to take back ([`AlwaysUse::agent`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The app whose leave to take back ([`AlwaysUse::app`]); with neither
    /// this nor `agent`, every leave of the item goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
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

/// One use by reference, as a catalogue offers it.
fn use_row(
    (provider, resource, label): (&str, &str, &str),
    (action, action_label): (&str, &str),
    description: String,
    params: Vec<crate::ActionParam>,
) -> CatalogueRow {
    let mut contract = crate::ActionContract {
        // It signs in somewhere outside the workspace.
        kind: crate::ActionKind::Outside,
        ..crate::ActionContract::default()
    };
    // It takes an item's address, which a person never picks from a list.
    contract.set_audience(crate::contract::Audience::Agents);
    CatalogueRow::for_action(
        provider,
        provider,
        crate::ActionDescriptor {
            did: None,
            state: crate::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(resource, action),
            resource_kind: resource.to_owned(),
            resource_label: label.to_owned(),
            action_label: action_label.to_owned(),
            keywords: Vec::new(),
            icon: None,
            description: Some(description),
            locus: crate::ExecutionLocus::ServerPreferred,
            kind: crate::OperationKind::Effect,
            contract,
            input: crate::InputSpec::None,
            params,
            view: None,
            // A use of another app's writes the use down in the vault; the vault's own
            // actions say what each writes.
            effects: if provider == PROVIDER {
                effects(action)
            } else {
                vec![vault_step(KEEP_USES)]
            },
        },
    )
}

/// The actions that use an item by reference, as a catalogue offers them to
/// an agent acting for the person: adding a mail account whose password the
/// vault keeps, and keeping a provider's key from it. Each takes the item's
/// reference ([`item_param`]) where the secret would stand, and says in its
/// description how the reference is come by, so a model that lists the
/// action reads how to ask and needs no instruction of its own.
#[must_use]
pub fn use_rows() -> Vec<CatalogueRow> {
    use crate::{ActionParam, ParamType};
    vec![
        use_row(
            (
                person_accounts::PROVIDER,
                person_accounts::RESOURCE,
                "Mail accounts",
            ),
            (
                person_accounts::ADD_MAIL_WITH_ITEM,
                "Add mail account from vault",
            ),
            format!(
                "Add a mail account for your person, signing in with a password their vault \
                 keeps. You never hold the password. To get it, call the vault's `ask` with \
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
            (
                model_providers::PROVIDER,
                model_providers::RESOURCE,
                "Model providers",
            ),
            (
                model_providers::SET_KEY_FROM_ITEM,
                "Keep provider key from vault",
            ),
            format!(
                "Keep the API key of one of your person's model providers from a key their \
                 vault keeps. You never hold the key. To get it, call the vault's `ask` with \
                 kind `key` and belongsTo `[\"<the web address of the provider's endpoint, \
                 such as https://api.example.com>\"]`: your person types it into a protected \
                 field, and you are told the `reference` the workspace's vault keeps it under. Pass that \
                 reference as `key`. The item must be a key that belongs to the provider's \
                 endpoint, and it is kept for no other. {ASKED_FIRST}"
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

/// The use by reference `target` names, as [`use_rows`] offers it; `None`
/// for every other action.
#[must_use]
pub fn use_of(target: &RemoteActionTarget) -> Option<crate::ActionDescriptor> {
    if *target == page_target() {
        return page_use();
    }
    use_rows().into_iter().find_map(|row| {
        let action = row.action?;
        (row.provider == target.provider && action.address == target.address).then_some(action)
    })
}

/// What [`FILL_PAGE`] names: the site the page is at, the item and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FillPage {
    /// The site the page is at: its origin, as its window's engine says it.
    pub site: String,
    /// The item, by its reference.
    pub item: ItemRef,
    /// Why whoever asks says it needs the item; empty when it gave none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// Where [`FILL_PAGE`] is addressed.
#[must_use]
pub fn page_target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, FILL_PAGE),
    }
}

/// [`FILL_PAGE`] as a person is shown it when they are asked first: the
/// site, and the item withheld.
#[must_use]
pub fn page_use() -> Option<crate::ActionDescriptor> {
    use crate::{ActionParam, ParamType};
    use_row(
        (PROVIDER, RESOURCE, "Vault"),
        (FILL_PAGE, "Fill page field from vault"),
        format!(
            "Fill a field of a web page from an item your person's vault keeps for that page's \
             site: a login's password, a code generator's code of this moment, or a kept \
             value. You never hold what the item keeps. {ASKED_FIRST}"
        ),
        vec![
            ActionParam::required(
                "site",
                ParamType::Text,
                "The site the page is at, as its window's engine names it.",
            ),
            item_param("item", REFERENCE),
            ActionParam::optional("reason", ParamType::Text, REASON),
        ],
    )
    .action
}

/// What a turn names when it asks for a field of the page in its workspace's Browser window
/// to be filled ([`FILL_PAGE`], as [`fill_row`] offers it): the field, the item and why. The
/// site is not the turn's to name: the window's engine says where its page is.
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

/// [`FILL_PAGE`] as a turn is offered it by whoever can hand the fill to its workspace's
/// Browser window: the turn names the field and the item, and the window names the site.
#[must_use]
pub fn fill_row() -> CatalogueRow {
    use crate::{ActionParam, ParamType};
    use_row(
        (PROVIDER, RESOURCE, "Vault"),
        (FILL_PAGE, "Fill page field from vault"),
        format!(
            "Fill a field of the page in this workspace's Browser window from an item your \
             person's vault keeps for that page's site: a login's password, a code generator's \
             code of this moment, or a kept value. You name the field as the page's outline \
             names it and the item by its reference, and you never hold what the item keeps. \
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

/// The word a leave names the use `target` by
/// ([`AlwaysUse::using`]): its provider and its action.
#[must_use]
pub fn use_word(target: &RemoteActionTarget) -> String {
    format!("{}/{}", target.provider, target.address.action)
}

/// A web request made with a key the vault keeps, by reference
/// ([`WebCall`]): the server adds the key on the way out and answers what
/// the service answered, the key struck out of it.
pub const WEB_CALL: &str = "web-call";

/// Content the workspace keeps, named by the address of its bytes: what a
/// call sends as a part, or as its whole body, without the bytes passing
/// through the asking app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebCallPart {
    /// The form field's name.
    pub name: String,
    /// The part's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The part's JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<serde_json::Value>,
    /// Content the workspace keeps, read from its store and sent as the part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<ContentRef>,
}

/// What [`WEB_CALL`] names: the request, and the item that is its key. The
/// key goes where its item says its service takes it, never where the
/// request says.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebCall {
    /// The address to call, at the service the key belongs to.
    pub url: String,
    /// When left out, `POST` for a call that sends something and `GET` for any other.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub method: String,
    /// Headers to send besides the key.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
    /// What to send, as text.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    /// What to send, as a multipart body of parts: text, JSON and content the
    /// workspace keeps. Never beside [`Self::body`] or [`Self::body_content`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body_parts: Vec<WebCallPart>,
    /// Content the workspace keeps, sent as the whole body. Never beside
    /// [`Self::body`] or [`Self::body_parts`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_content: Option<ContentRef>,
    /// The key, by its reference.
    pub key: ItemRef,
    /// Why whoever asks says it needs the call; empty when it gave none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// Where [`WEB_CALL`] is addressed.
#[must_use]
pub fn call_target() -> RemoteActionTarget {
    target(WEB_CALL)
}

/// Whether `target` is a [`WEB_CALL`] at the vault's own resource: the call an app's held
/// connection makes, never an asked record kept about one.
#[must_use]
pub fn is_web_call(target: &RemoteActionTarget) -> bool {
    *target == call_target()
}

/// The actions that send what an item keeps out with a request of the
/// agent's own making, as a catalogue offers them to an agent acting for
/// the person: a web call with a key. They stand apart from [`use_rows`]
/// because no "Always" stands for them: what such a request does is the
/// agent's to choose each time, so its person is shown each one.
#[must_use]
pub fn call_rows() -> Vec<CatalogueRow> {
    use crate::{ActionParam, ParamType};
    vec![use_row(
        (PROVIDER, RESOURCE, "Vault"),
        (WEB_CALL, "Call a service with a key from the vault"),
        format!(
            "Make a web request to a service that takes a key, with a key your person's vault \
             keeps: for a service that has no connection of its own. The server adds the key \
             on the way out, where the item says its service takes it (as a bearer token \
             unless it says otherwise), and only to the service the item belongs to: `url` \
             must be at that service's web address, over https. You never hold the key, and \
             it is struck from what you are answered: the status, the headers and the body. \
             What it sends is `body` text, `bodyParts` (an inline `text` or `json` part, or a \
             `content` part naming content your workspace keeps by its BLAKE3 hex address, with \
             its filename and content type), or `bodyContent`, one content reference sent as the \
             whole body. The server reads that content from the workspace's store: its bytes \
             never pass through you, and you may name only content the workspace keeps. \
             To get the key, call the vault's `ask` with kind `key`, belongsTo `[\"<the \
             service's web address, such as https://api.example.com>\"]` and `sentIn`, which \
             says how the service takes its key when not as a bearer token: `basic`, \
             `header:<name>` such as `header:X-Api-Key`, or `query:<name>`. Your person types \
             it into a protected field, and you are told the `reference` the workspace's vault keeps it \
             under. Pass that reference as `key`. {CALL_ASKED_FIRST}"
        ),
        vec![
            ActionParam::required("url", ParamType::Text, "The address to call."),
            ActionParam::optional(
                "method",
                ParamType::Text,
                "GET, HEAD, POST, PUT, PATCH or DELETE; when left out, POST if it sends a body and GET if not.",
            ),
            ActionParam::optional(
                "headers",
                ParamType::Object,
                "Headers to send besides the key, by name. Not the one the key rides in, and \
                 none that signs the request in.",
            ),
            ActionParam::optional(
                "body",
                ParamType::Text,
                "What to send, as text. Not beside `bodyParts` or `bodyContent`.",
            ),
            ActionParam::optional(
                "bodyParts",
                ParamType::Array,
                "What to send, as the parts of a multipart body: each names its field and holds \
                 `text`, `json`, or `content`, content the workspace keeps by its BLAKE3 hex \
                 address with a `filename` and `contentType`. The vault reads the bytes from the \
                 workspace's store, so they never pass through you.",
            ),
            ActionParam::optional(
                "bodyContent",
                ParamType::Object,
                "Content the workspace keeps, by its BLAKE3 hex address with a `filename` and \
                 `contentType`, sent as the whole body.",
            ),
            item_param("key", REFERENCE),
            ActionParam::optional("reason", ParamType::Text, REASON),
        ],
    )]
}

/// Hand one item's secret to a process of an app's own that speaks the item's protocol itself
/// (a gateway login, voice, an SSH or VNC handshake, a database login), addressed at the item:
/// `vault.item:<id>#release` ([`release_target`]), so the grant its person gives on it is a grant
/// on that item alone. It takes no arguments and answers [`Released`].
///
/// Only a sandboxed process of the app, which a turn of work placed on a device that may read
/// the item, receives it: the app's held connection. The secret is answered on that process's
/// connection alone, and nothing keeps it: no record, argument, log or history. Every other use
/// of an item stays a call the vault makes itself, such as [`WEB_CALL`].
pub const RELEASE: &str = "release";

/// Where [`RELEASE`] of `item` is addressed: the item itself.
#[must_use]
pub fn release_target(item: &ItemRef) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(item.to_string(), RELEASE),
    }
}

/// Whether `target` is a [`RELEASE`], of whatever it is addressed at.
#[must_use]
pub fn is_release(target: &RemoteActionTarget) -> bool {
    target.provider == PROVIDER && target.address.action == RELEASE
}

/// The item a [`RELEASE`] at `target` is addressed at; `None` for any other call, or a release
/// addressed at something that is no item.
#[must_use]
pub fn released_item(target: &RemoteActionTarget) -> Option<ItemRef> {
    is_release(target)
        .then(|| ItemRef::parse(&target.address.resource))
        .flatten()
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

/// What [`FIND`] names: the app and the purpose whose tags the item carries. The app passes its
/// own tags; no publisher or name maps one to the other.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FindItem {
    pub app: String,
    pub purpose: String,
}

/// What [`FIND`] answers: the item's reference, or `None` when the vault keeps no item tagged
/// with that app and purpose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Found {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<ItemRef>,
}

/// Whether `target` is a [`FIND`].
#[must_use]
pub fn is_find(target: &RemoteActionTarget) -> bool {
    target.provider == PROVIDER && target.address.action == FIND
}

/// The logins the vault keeps for one site ([`PageLoginsAt`]), as [`PageLogins`]: what is safe to
/// say of each, the name it signs in with included, never its secret.
///
/// This and the other page login actions ([`KEEP_PAGE_LOGIN`], [`FORGET_PAGE_LOGIN`] and
/// [`PAGE_FILL`]) are answered only to a window of an app's own that shows a page, on that
/// window's own connection, which the device's daemon knows by the process it started: the site
/// is the one the window's engine says its page is at, and a secret travels between that window
/// and the vault alone. Every other caller is refused.
pub const PAGE_LOGINS: &str = "page-logins";
/// Keep the login a page sent ([`KeepPageLogin`]) as the app's own, for signing in at that site,
/// answering what is safe to say of it as a [`PageLogin`].
pub const KEEP_PAGE_LOGIN: &str = "keep-page-login";
/// Forget a login ([`ForgetPageLogin`]) the app kept, or one it may fill, answering the
/// [`PageLogins`] that remain for the site.
pub const FORGET_PAGE_LOGIN: &str = "forget-page-login";
/// Hand one login's name and secret to the window whose page is at a site the login belongs to,
/// for its engine to fill the page's form ([`PageFillAt`], answered [`PageFilled`]). Addressed at
/// the item (`vault.item:<id>#page-fill`, [`page_fill_target`]), so the grant its person gives on
/// it is a grant on that item alone. A login the app kept itself needs no grant; any other is
/// asked of its person once, and the window asks again once they allowed it.
pub const PAGE_FILL: &str = "page-fill";

/// What the app kept a page's login for, as the item names it ([`KEEP_PAGE_LOGIN`]).
pub const SIGN_IN: &str = "sign-in";

/// Where [`PAGE_FILL`] of `item` is addressed: the item itself.
#[must_use]
pub fn page_fill_target(item: &ItemRef) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(item.to_string(), PAGE_FILL),
    }
}

/// Whether `target` is one of the page login actions, which only a window of an app's own is
/// answered.
#[must_use]
pub fn is_page_login(target: &RemoteActionTarget) -> bool {
    target.provider == PROVIDER
        && match target.address.action.as_str() {
            PAGE_LOGINS | KEEP_PAGE_LOGIN | FORGET_PAGE_LOGIN => {
                target.address.resource == RESOURCE
            }
            PAGE_FILL => true,
            _ => false,
        }
}

/// The item a [`PAGE_FILL`] at `target` is addressed at; `None` for any other call, or one
/// addressed at something that is no item.
#[must_use]
pub fn page_filled_item(target: &RemoteActionTarget) -> Option<ItemRef> {
    (target.provider == PROVIDER && target.address.action == PAGE_FILL)
        .then(|| ItemRef::parse(&target.address.resource))
        .flatten()
}

/// What [`PAGE_LOGINS`] names: the site the page is at, as its window's engine says it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageLoginsAt {
    pub site: String,
}

/// What is safe to say of one login a page is offered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageLogins {
    pub logins: Vec<PageLogin>,
}

/// What [`KEEP_PAGE_LOGIN`] names: the site the page is at, the name and secret its form sent,
/// and the login it replaces, if the person chose one. The secret travels here and nowhere back.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeepPageLogin {
    pub site: String,
    pub username: String,
    pub secret: String,
    pub replacing: Option<ItemRef>,
}

impl std::fmt::Debug for KeepPageLogin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeepPageLogin")
            .field("site", &self.site)
            .field("replacing", &self.replacing)
            .finish_non_exhaustive()
    }
}

/// What [`FORGET_PAGE_LOGIN`] names: the site the page is at and the login to forget.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForgetPageLogin {
    pub site: String,
    pub item: ItemRef,
}

/// What [`PAGE_FILL`] names: the site the page is at, as its window's engine says it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageFillAt {
    pub site: String,
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
/// in with. It holds no secret, and nothing in it is worded by the agent: the person reads the
/// system's words for it ([`Self::words`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AskForItem {
    pub kind: ItemKind,
    /// Where it belongs, each a web address or a server ([`looks_like_place`]): the only places
    /// it is ever handed to.
    pub belongs_to: Vec<String>,
    /// The name it signs in with, when it has one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    /// Where a key rides in a request to its service, as [`AddItem::sent_in`] says it.
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

/// What [`ASK`] is answered with: the item the person's vault now keeps, or none where they
/// declined.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Asked {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<ItemRef>,
}

/// The target of [`ASK`].
#[must_use]
pub fn ask_target() -> RemoteActionTarget {
    target(ASK)
}

/// [`ASK`] as a catalogue offers it to an agent acting for the person.
#[must_use]
pub fn ask_row() -> CatalogueRow {
    use crate::{ActionParam, ParamType};
    use_row(
        (PROVIDER, RESOURCE, "Vault"),
        (ASK, "Ask your person for a secret"),
        "Ask your person for a password, a key or another secret. This is the one way to get \
         one: never ask for one in the conversation, where everyone in it and you read what \
         they write. Your person types it into the system's protected field on their own device, \
         in words the system writes; you never see it. The workspace's vault keeps it for the places in \
         `belongsTo` alone: a site as `https://example.com`, a server as \
         `imap://mail.example.com:993` or `ssh://git.example.com`. You are told the \
         `reference` it is kept under (`vault.item:…`), or that they declined: pass the \
         reference where an action takes the secret. Where nobody watches, the call ends at \
         once and you are told in your conversation with them; do not make it again."
            .to_owned(),
        vec![
            ActionParam::required(
                "kind",
                ParamType::Text,
                "What it is: `login` (a password), `key`, `code-generator`, `ssh-key` or `value`.",
            ),
            ActionParam::required(
                "belongsTo",
                ParamType::Array,
                "Where it belongs: each a web address or a server, at most four.",
            ),
            ActionParam::optional(
                "username",
                ParamType::Text,
                "The name it signs in with, when you know it.",
            ),
            ActionParam::optional(
                "sentIn",
                ParamType::Text,
                "For a key: how its service takes it when not as a bearer token: `basic`, \
                 `header:<name>` or `query:<name>`.",
            ),
        ],
    )
}

/// What every call that sends an item's secret out says of being asked
/// first.
const CALL_ASKED_FIRST: &str = "When you work with nobody watching, your person is asked first \
     in the conversation whether you may make this call, and nothing is sent: tell them it \
     waits for their answer. Once they allowed it, make the same call again with the same \
     arguments: it is sent once. Each call is asked about anew.";

/// The use by reference `target` names, as [`use_rows`] or [`call_rows`]
/// offers it; `None` for every other action.
#[must_use]
pub fn described_use(target: &RemoteActionTarget) -> Option<crate::ActionDescriptor> {
    use_of(target).or_else(|| {
        call_rows().into_iter().find_map(|row| {
            let action = row.action?;
            (row.provider == target.provider && action.address == target.address).then_some(action)
        })
    })
}

#[cfg(test)]
mod call_tests {
    use super::*;

    /// A web call takes its key by reference and never as text, says how
    /// the reference is come by, and is listed apart from the uses an
    /// "Always" may stand for.
    #[test]
    fn a_web_call_takes_its_key_by_reference_and_is_listed_apart() {
        let rows = call_rows();
        assert_eq!(rows.len(), 1);
        let target = call_target();
        assert_eq!(use_word(&target), "tilo.vault/web-call");
        let action = rows[0].action.clone().expect("the row is an action");
        assert_eq!(rows[0].provider, PROVIDER);
        assert_eq!(action.address, target.address);
        assert_eq!(action.contract.kind, crate::ActionKind::Outside);
        assert_eq!(
            action
                .params
                .iter()
                .filter(|param| takes_item(param))
                .count(),
            1
        );
        assert!(
            action
                .params
                .iter()
                .all(|param| param.ty != crate::ParamType::Secret),
            "no parameter takes a secret's text"
        );
        let described = action.description.unwrap_or_default();
        for said in [
            "vault's `ask`",
            "sentIn",
            "`reference`",
            "asked first",
            "struck",
        ] {
            assert!(described.contains(said), "{said}: {described}");
        }
        assert_eq!(
            described_use(&target).map(|found| found.address),
            Some(target.address.clone())
        );
        assert!(use_of(&target).is_none(), "no Always stands for a call");
        assert!(
            use_rows()
                .iter()
                .all(|row| described_use(&RemoteActionTarget {
                    provider: row.provider.clone(),
                    address: row.action.as_ref().unwrap().address.clone(),
                })
                .is_some()),
            "the uses are described as before"
        );
        assert!(described_use(&super::target(LIST)).is_none());
    }

    /// A call's arguments read as typed, and nothing unknown rides along.
    #[test]
    fn a_calls_arguments_are_typed() {
        let reference = format!("vault.item:{}", "0".repeat(32));
        let call: WebCall = serde_json::from_value(serde_json::json!({
            "url": "https://api.example.test/v1/balance",
            "key": reference,
        }))
        .unwrap();
        assert!(call.method.is_empty() && call.headers.is_empty() && call.body.is_empty());
        let whole: WebCall = serde_json::from_value(serde_json::json!({
            "url": "https://api.example.test/v1/notes",
            "method": "POST",
            "headers": {"Content-Type": "application/json"},
            "body": "{}",
            "key": reference,
            "reason": "To file the note.",
        }))
        .unwrap();
        assert_eq!(whole.headers["Content-Type"], "application/json");
        let parts: WebCall = serde_json::from_value(serde_json::json!({
            "url": "https://api.example.test/v1/upload",
            "method": "POST",
            "bodyParts": [
                {"name": "payload_json", "json": {"content": "hi"}},
                {"name": "files[0]", "content": {
                    "app": "so.tilo.chat",
                    "hash": "ab".repeat(32),
                    "filename": "a.png",
                    "contentType": "image/png",
                }},
                {"name": "note", "text": "one line"},
            ],
            "key": reference,
        }))
        .unwrap();
        assert_eq!(parts.body_parts.len(), 3);
        assert_eq!(
            parts.body_parts[0].json,
            Some(serde_json::json!({"content": "hi"}))
        );
        let content = parts.body_parts[1].content.clone().unwrap();
        assert_eq!(content.app, "so.tilo.chat");
        assert_eq!(content.filename, "a.png");
        assert_eq!(content.content_type, "image/png");
        let raw: WebCall = serde_json::from_value(serde_json::json!({
            "url": "https://api.example.test/v1/upload",
            "method": "PUT",
            "bodyContent": {"hash": "ab".repeat(32), "contentType": "image/png"},
            "key": reference,
        }))
        .unwrap();
        assert_eq!(raw.body_content.unwrap().hash, "ab".repeat(32));
        for wrong in [
            serde_json::json!({"url": "https://api.example.test", "key": "a key typed out"}),
            serde_json::json!({"url": "https://api.example.test", "key": reference, "secret": "x"}),
            serde_json::json!({"key": reference}),
            serde_json::json!({"url": "https://api.example.test", "key": reference,
                "bodyParts": [{"name": "x", "text": "t", "other": 1}]}),
        ] {
            assert!(serde_json::from_value::<WebCall>(wrong).is_err());
        }
        let adding: AddItem = serde_json::from_value(serde_json::json!({
            "kind": "key",
            "belongsTo": ["https://api.example.test"],
            "sentIn": "header:X-Api-Key",
            "secret": "made-up-key-0123456789",
        }))
        .unwrap();
        assert_eq!(adding.sent_in, "header:X-Api-Key");
        assert!(!format!("{adding:?}").contains("made-up-key"));
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

    const ID: &str = "0123456789abcdef0123456789abcdef";

    /// An ask names only places a secret can belong to, and the person reads the system's
    /// words for it, which say where it goes and who never sees it.
    #[test]
    fn an_ask_for_a_secret_is_checked_and_worded_by_the_system() {
        let ask: AskForItem = serde_json::from_value(serde_json::json!({
            "kind": "login",
            "belongsTo": [" https://example.com "],
            "username": "ada",
        }))
        .unwrap();
        let ask = ask.checked().unwrap();
        assert_eq!(
            ask.words("Scout"),
            "Scout asks for the password of ada at https://example.com. Scout never sees what \
             you type: your vault keeps it for that place alone."
        );
        for wrong in [
            serde_json::json!({"kind": "login", "belongsTo": []}),
            serde_json::json!({"kind": "login", "belongsTo": ["my bank"]}),
            serde_json::json!({"kind": "login", "belongsTo": ["https://a.example"], "sentIn": "basic"}),
        ] {
            let ask: AskForItem = serde_json::from_value(wrong).unwrap();
            assert!(ask.checked().is_err());
        }
        assert!(
            serde_json::from_value::<AskForItem>(serde_json::json!({
                "kind": "login",
                "belongsTo": ["https://example.com"],
                "secret": "hunter2",
            }))
            .is_err(),
            "an ask carries no secret"
        );
        let row = ask_row();
        let action = row.action.unwrap();
        assert_eq!(action.address.action, ASK);
        assert!(
            action
                .params
                .iter()
                .all(|param| param.ty != crate::ParamType::Secret && !takes_item(param)),
            "an ask takes no secret and no reference"
        );
    }

    /// What could name a place passes the first check, and plain words,
    /// a bare host and another scheme do not.
    #[test]
    fn a_place_is_checked_before_a_person_is_asked() {
        for place in [
            "https://example.com/login",
            "HTTP://example.com",
            "imap://mail.example.com:993",
            "smtp://mail.example.com:465",
            "ssh://git.example.com",
        ] {
            assert!(looks_like_place(place), "{place}");
        }
        for other in [
            "",
            "example.com",
            "my bank",
            "ftp://example.com",
            "https://",
            "https://exa mple.com",
        ] {
            assert!(!looks_like_place(other), "{other:?}");
        }
    }

    /// Each use by reference takes the item's address where the secret
    /// would stand and no secret anywhere, tells an agent how the reference
    /// is come by, and is found again by its target and by no other.
    #[test]
    fn a_use_by_reference_takes_no_secret_and_says_how_to_ask() {
        let rows = use_rows();
        assert_eq!(rows.len(), 2);
        for row in &rows {
            let action = row.action.as_ref().unwrap();
            assert_eq!(
                action
                    .params
                    .iter()
                    .filter(|param| takes_item(param))
                    .count(),
                1,
                "{}",
                action.address.action
            );
            assert!(
                !action
                    .params
                    .iter()
                    .any(|param| param.ty == crate::ParamType::Secret)
            );
            assert_eq!(action.contract.kind, crate::ActionKind::Outside);
            let description = action.description.as_deref().unwrap();
            for said in ["vault's `ask`", "`reference`", "asked first", "Always"] {
                assert!(description.contains(said), "{said}: {description}");
            }
            let target = RemoteActionTarget {
                provider: row.provider.clone(),
                address: action.address.clone(),
            };
            assert_eq!(
                use_of(&target).map(|found| found.address),
                Some(action.address.clone())
            );
            assert_eq!(
                use_word(&target),
                format!("{}/{}", row.provider, action.address.action)
            );
        }
        assert!(use_of(&target(LIST)).is_none());
        assert!(use_of(&person_accounts::target(person_accounts::ADD_MAIL)).is_none());
        assert!(use_of(&model_providers::target(model_providers::SET_KEY)).is_none());

        // A review of a use shows the reference withheld, as it shows a secret.
        let mail = use_of(&person_accounts::target(
            person_accounts::ADD_MAIL_WITH_ITEM,
        ))
        .unwrap();
        let arguments = serde_json::json!({
            "address": "ada@example.test",
            "incomingHost": "imap.example.test",
            "outgoingHost": "smtp.example.test",
            "password": format!("vault.item:{ID}"),
        });
        let draft = crate::Draft::render(&mail, arguments.as_object().unwrap());
        let shown = serde_json::to_string(&draft).unwrap();
        assert!(!shown.contains(ID), "{shown}");
        assert!(shown.contains("ada@example.test"), "{shown}");
    }

    /// What an item remembers reads with and without leaves.
    #[test]
    fn leaves_are_listed_beside_the_uses() {
        let none: ItemUses = serde_json::from_value(serde_json::json!({"uses": []})).unwrap();
        assert!(none.always.is_empty());
        assert_eq!(
            serde_json::to_value(&none).unwrap(),
            serde_json::json!({"uses": []})
        );
        let leave = AlwaysUse {
            agent: "scout".into(),
            app: String::new(),
            by: "Scout".into(),
            using: "tilo.accounts/add-mail-with-item".into(),
            places: vec!["imap://mail.example.test:993".into()],
            since: 7,
        };
        let some = ItemUses {
            uses: Vec::new(),
            always: vec![leave],
        };
        let said = serde_json::to_value(&some).unwrap();
        assert_eq!(said["always"][0]["agent"], "scout");
        assert_eq!(said["always"][0]["since"], 7);
        assert!(
            said["always"][0].get("app").is_none(),
            "an agent's leave names no app"
        );
        assert_eq!(serde_json::from_value::<ItemUses>(said).unwrap(), some);
        let back: TakeBack =
            serde_json::from_value(serde_json::json!({"item": format!("vault.item:{ID}")}))
                .unwrap();
        assert_eq!(back.agent, None);
    }

    /// A use says what it was for where that was written down, and reads
    /// without it where it was not.
    #[test]
    fn a_use_says_what_it_was_for_where_that_is_known() {
        let plain: ItemUse = serde_json::from_value(serde_json::json!({
            "at": 7,
            "places": ["https://api.example.test"],
        }))
        .unwrap();
        assert!(plain.using.is_empty() && plain.what.is_empty() && plain.allowed.is_empty());
        assert_eq!(
            serde_json::to_value(&plain).unwrap(),
            serde_json::json!({"at": 7, "places": ["https://api.example.test"]})
        );
        let call = ItemUse {
            at: 8,
            places: vec!["https://api.example.test".into()],
            by: "Scout".into(),
            using: "tilo.vault/web-call".into(),
            what: "GET /v1/balance".into(),
            allowed: "once".into(),
        };
        let said = serde_json::to_value(&call).unwrap();
        assert_eq!(said["using"], "tilo.vault/web-call");
        assert_eq!(said["what"], "GET /v1/balance");
        assert_eq!(said["allowed"], "once");
        assert_eq!(serde_json::from_value::<ItemUse>(said).unwrap(), call);
    }

    /// A reference is an item's address and nothing else reads as one.
    #[test]
    fn a_reference_is_an_items_address() {
        let reference = ItemRef::from_id(ID).unwrap();
        assert_eq!(reference.to_string(), format!("vault.item:{ID}"));
        assert_eq!(
            ItemRef::parse(&reference.to_string()),
            Some(reference.clone())
        );
        assert_eq!(reference.id(), ID);
        let json = serde_json::to_value(&reference).unwrap();
        assert_eq!(json, serde_json::json!(format!("vault.item:{ID}")));
        assert_eq!(serde_json::from_value::<ItemRef>(json).unwrap(), reference);

        for other in [
            String::new(),
            ID.to_owned(),
            format!("vault.item:{}", ID.to_uppercase()),
            format!("vault.item:{ID}0"),
            format!("vault.item:{}", ID.get(1..).unwrap()),
            format!("vault.items:{ID}"),
            format!("vault.item:{ID}/secret"),
            "vault.item:../../people".to_owned(),
        ] {
            assert_eq!(ItemRef::parse(&other), None, "{other}");
            assert!(
                serde_json::from_value::<ItemRef>(serde_json::json!(other)).is_err(),
                "{other}"
            );
        }
    }

    /// A parameter that takes a secret by reference is a resource parameter
    /// of the vault item's kind, and what binds as a reference there is an
    /// item's address, never typed text.
    #[test]
    fn a_parameter_takes_a_secret_by_reference() {
        let param = item_param("password", "The account's password, by reference");
        assert!(takes_item(&param));
        assert_eq!(param.ty, crate::ParamType::Resource);
        let typed = crate::ActionParam::required("password", crate::ParamType::Secret, "typed");
        assert!(!takes_item(&typed));
        let other = crate::ActionParam::required("folder", crate::ParamType::Resource, "a folder")
            .of_resource_kind("files.entry");
        assert!(!takes_item(&other));

        let params = [param];
        let named = serde_json::json!({"password": format!("vault.item:{ID}")});
        let bound = crate::ActionArgs::bind_json(&params, named.as_object().unwrap()).unwrap();
        assert_eq!(ItemRef::bound(&bound, "password"), ItemRef::from_id(ID));
        assert_eq!(ItemRef::bound(&bound, "username"), None);
        let typed = serde_json::json!({"password": "hunter2"});
        let bound = crate::ActionArgs::bind_json(&params, typed.as_object().unwrap());
        // Text that is no address either does not bind or binds as no reference.
        assert!(!bound.is_ok_and(|bound| ItemRef::bound(&bound, "password").is_some()));
    }

    /// A secret never reaches a log, and no reading has a field that could
    /// carry one: a view says the kind, the places, and the two times.
    #[test]
    fn nothing_read_back_carries_a_secret() {
        let adding: AddItem = serde_json::from_value(serde_json::json!({
            "kind": "login",
            "belongsTo": ["https://example.com"],
            "username": "ada@example.test",
            "secret": "hunter2",
        }))
        .unwrap();
        let printed = format!("{adding:?}");
        assert!(
            !printed.contains("hunter2") && !printed.contains("ada@"),
            "{printed}"
        );

        let view = ItemView {
            reference: ItemRef::from_id(ID).unwrap(),
            kind: ItemKind::CodeGenerator,
            belongs_to: vec!["https://example.com".into()],
            title: Some("Example".into()),
            added_at: 1_800_000_000,
            last_used_at: Some(1_800_000_100),
            algorithm: None,
            digits: None,
            period: None,
            tags: Vec::new(),
            fields: Vec::new(),
            has_notes: false,
            changed_at: 1_800_000_000,
            concerns: Vec::new(),
        };
        let json = serde_json::to_value(KeptItems { items: vec![view] }).unwrap();
        let mut fields: Vec<&str> = json["items"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            [
                "addedAt",
                "belongsTo",
                "changedAt",
                "kind",
                "lastUsedAt",
                "reference",
                "title"
            ]
        );
        assert_eq!(json["items"][0]["kind"], "code-generator");
        // A view with a field of its own making is not one.
        assert!(
            serde_json::from_value::<ItemView>(serde_json::json!({
                "reference": format!("vault.item:{ID}"),
                "kind": "login",
                "belongsTo": [],
                "addedAt": 0,
                "secret": "hunter2",
            }))
            .is_err()
        );
    }

    /// A page's field filled from an item is a use a leave and a rule can
    /// name and a person can be shown, and no catalogue offers it.
    #[test]
    fn filling_a_page_is_a_use_no_catalogue_offers() {
        let target = page_target();
        assert_eq!(use_word(&target), "tilo.vault/fill-page");
        let shown = use_of(&target).expect("a person is shown what it is");
        assert_eq!(shown.address, target.address);
        assert!(shown.params.iter().any(takes_item));
        assert!(
            use_rows()
                .iter()
                .all(|row| row.action.as_ref().map(|action| &action.address)
                    != Some(&target.address)),
            "no turn is offered it by a listing"
        );
        let fill: FillPage = serde_json::from_value(serde_json::json!({
            "site": "https://example.test",
            "item": format!("vault.item:{}", "0".repeat(32)),
        }))
        .unwrap();
        assert_eq!(fill.site, "https://example.test");
        assert!(fill.reason.is_empty());
    }

    /// What a turn is offered names the field and the item by reference, and never the site:
    /// where the page is, is its window's to say.
    #[test]
    fn a_turn_names_the_field_and_the_item_and_never_the_site() {
        let row = fill_row();
        assert_eq!(row.provider, PROVIDER);
        let offered = row.action.expect("the row offers the fill");
        assert_eq!(offered.address, page_target().address);
        assert_eq!(offered.params.len(), 3);
        assert!(offered.params.iter().any(takes_item));
        let item = format!("vault.item:{}", "0".repeat(32));
        let named: FillField =
            serde_json::from_value(serde_json::json!({"element": "d1e5", "item": item})).unwrap();
        assert_eq!(named.element, "d1e5");
        assert!(named.reason.is_empty());
        assert!(
            serde_json::from_value::<FillField>(serde_json::json!({
                "element": "d1e5",
                "item": item,
                "site": "https://example.test",
            }))
            .is_err(),
            "a turn that names a site is not read"
        );
    }
}

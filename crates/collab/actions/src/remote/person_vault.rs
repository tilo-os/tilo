//! What a person keeps to sign in with, kept where their workspace runs.
//!
//! A login, a key, a code generator's seed, an ssh key or a plain value such
//! as a card number is an item of the person's own vault on the server that
//! runs their workspace, bound to where it belongs: a site, a service, a
//! server. Only the person's own device
//! adds one, lists them or removes one; an agent's request is refused. A
//! secret travels in with [`ADD`] and never back: a listing says an item's
//! kind, where it belongs, when it was added and when it was last used, and
//! nothing else.
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

use super::{ActionAddress, CatalogueRow, RemoteActionTarget, person_accounts, person_models};

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

/// The use that fills a field of a web page from an item, by reference
/// ([`FillPage`]): the action a leave names it by ([`use_word`]) and a
/// narrowed agent's rule names. No catalogue offers it by itself
/// ([`use_rows`]): whoever holds the page's window takes it for the turn
/// that asked, with the site the window's engine says the page is at.
pub const FILL_PAGE: &str = "fill-page";

/// The kind of resource a reference is the address of: what a parameter that
/// takes a secret by reference names as its resource kind.
pub const ITEM_KIND: &str = "vault.item";

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
    /// The reference to the item `id` names, when `id` is an item's id: 32
    /// lowercase hexadecimal digits.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        (id.len() == 32
            && id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .then(|| Self { id: id.to_owned() })
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
    /// When it was added, in seconds since the Unix epoch.
    pub added_at: u64,
    /// When it was last handed to something that signs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<u64>,
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
    pub kind: ItemKind,
    /// Where it belongs, each a web address or a server; see
    /// [`ItemView::belongs_to`]. A page's address names its site.
    pub belongs_to: Vec<String>,
    /// The name it signs in with, when it has one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    /// Where a key rides in a request to its service, when not as a bearer
    /// token: `basic`, `header:<name>`, `header:<name>:<word before the
    /// key>` or `query:<name>`. Only a key says so.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sent_in: String,
    pub secret: String,
}

impl std::fmt::Debug for AddItem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AddItem")
            .field("kind", &self.kind)
            .field("belongs_to", &self.belongs_to)
            .finish_non_exhaustive()
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
    /// The agent, by its id: what [`TakeBack::agent`] names.
    pub agent: String,
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

/// The item whose leaves to take back: every one, or only those
/// of one agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TakeBack {
    pub item: ItemRef,
    /// The agent whose leaves to take back ([`AlwaysUse::agent`]); every
    /// agent's when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// What every use by reference says of the reference it takes.
const REFERENCE: &str = "The `reference` a question of kind `secret` was answered with \
     (`vault.item:…`). It names the vault item and is never the secret itself.";
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
                 keeps. You never hold the password. To get it, ask with Chat's `ask`, kind \
                 `secret`, detail `{{kind: \"login\", belongsTo: [\"imap://<incomingHost>: \
                <incomingPort>\", \"smtp://<outgoingHost>:<outgoingPort>\"], username}}`: your \
                 person types it into a protected field, and the answer `filled` carries a \
                 `reference` in its detail. Pass that reference as `password`. The item must \
                 be a login that belongs to both servers, and it is sent nowhere else. \
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
                person_models::PROVIDER,
                person_models::RESOURCE,
                "Model providers",
            ),
            (
                person_models::SET_KEY_FROM_ITEM,
                "Keep provider key from vault",
            ),
            format!(
                "Keep the API key of one of your person's model providers from a key their \
                 vault keeps. You never hold the key. To get it, ask with Chat's `ask`, kind \
                 `secret`, detail `{{kind: \"key\", belongsTo: [\"<the web address of the \
                 provider's endpoint, such as https://api.example.com>\"]}}`: your person types \
                 it into a protected field, and the answer `filled` carries a `reference` in \
                 its detail. Pass that reference as `key`. The item must be a key that belongs \
                 to the provider's endpoint, and it is kept for no other. {ASKED_FIRST}"
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

/// What [`WEB_CALL`] names: the request, and the item that is its key. The
/// key goes where its item says its service takes it, never where the
/// request says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebCall {
    /// The address to call, at the service the key belongs to.
    pub url: String,
    /// `GET` when left out.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub method: String,
    /// Headers to send besides the key.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
    /// What to send, as text.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
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
             To get the key, ask with Chat's `ask`, kind `secret`, detail `{{kind: \"key\", \
             belongsTo: [\"<the service's web address, such as https://api.example.com>\"], \
             sentIn}}`, where `sentIn` says how the service takes its key when not as a bearer \
             token: `basic`, `header:<name>` such as `header:X-Api-Key`, or `query:<name>`. \
             Your person types it into a protected field, and the answer `filled` carries a \
             `reference` in its detail. Pass that reference as `key`. {CALL_ASKED_FIRST}"
        ),
        vec![
            ActionParam::required("url", ParamType::Text, "The address to call."),
            ActionParam::optional(
                "method",
                ParamType::Text,
                "GET, HEAD, POST, PUT, PATCH or DELETE; GET when left out.",
            ),
            ActionParam::optional(
                "headers",
                ParamType::Object,
                "Headers to send besides the key, by name. Not the one the key rides in, and \
                 none that signs the request in.",
            ),
            ActionParam::optional("body", ParamType::Text, "What to send, as text."),
            item_param("key", REFERENCE),
            ActionParam::optional("reason", ParamType::Text, REASON),
        ],
    )]
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
            "kind `secret`",
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
        for wrong in [
            serde_json::json!({"url": "https://api.example.test", "key": "a key typed out"}),
            serde_json::json!({"url": "https://api.example.test", "key": reference, "secret": "x"}),
            serde_json::json!({"key": reference}),
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
            for said in ["kind `secret`", "`reference`", "asked first", "Always"] {
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
        assert!(use_of(&person_models::target(person_models::SET_KEY)).is_none());

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
            added_at: 1_800_000_000,
            last_used_at: Some(1_800_000_100),
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
            ["addedAt", "belongsTo", "kind", "lastUsedAt", "reference"]
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

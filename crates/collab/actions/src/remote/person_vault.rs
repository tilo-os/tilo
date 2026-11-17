//! What a person keeps to sign in with, kept where their workspace runs.
//!
//! A login, a key, a code generator's seed, an ssh key or a plain value such
//! as a card number is an item of the person's own vault on the host that
//! runs their workspace, bound to where it belongs: a site, a service, a
//! server. Only the person's own device
//! adds one, lists them or removes one; an agent's request is refused. A
//! secret travels in with [`ADD`] and never back: a listing says an item's
//! kind, where it belongs, when it was added and when it was last used, and
//! nothing else.
//!
//! What an agent holds is a reference ([`ItemRef`]): the address of an item,
//! which is not its secret. An action that needs a secret takes one as a
//! parameter ([`item_param`]). Whatever signs in resolves it on the host,
//! where the secret is used, and only at a place the item belongs to; each
//! use is remembered with when, where and for whom ([`USES`]), never with
//! what was handed.

use serde::{Deserialize, Serialize};

use super::{ActionAddress, RemoteActionTarget};

/// The provider the host answers the person's vault under.
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
/// When and where an item was handed to something that signs in
/// ([`UsesOf`]), as an [`ItemUses`].
pub const USES: &str = "uses";

/// The kind of resource a reference is the address of: what a parameter that
/// takes a secret by reference names as its resource kind.
pub const ITEM_KIND: &str = "vault.item";

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
/// An agent may hold one, pass it as an argument and keep it in a record.
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
}

/// The uses an item remembers, oldest first: the newest of them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemUses {
    pub uses: Vec<ItemUse>,
}

/// The item to forget.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveItem {
    pub item: ItemRef,
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
}

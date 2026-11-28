//! What every vocabulary knows of the workspace's vault, whose provider declares its actions
//! (`tilo_workspace_records::vault`): its provider, the reference that names one of
//! its items without its secret ([`ItemRef`]) and the parameter that takes one
//! ([`item_param`]), and the steps its actions take in the record the vault is kept in. Which of
//! its actions take which step, and which answer a secret, its declaration says
//! (`tilo_workspace_records::vault`).

use serde::{Deserialize, Serialize};

use super::{ActionAddress, InvokeArgument, InvokeRequest, RemoteActionTarget};

/// The provider the workspace's vault answers under.
pub const VAULT_PROVIDER: &str = "tilo.vault";
/// The resource the vault's own actions are of.
pub const VAULT_RESOURCE: &str = "vault";
/// The kind of resource a reference is the address of ([`ItemRef`]): what a parameter that
/// takes a secret by reference names as its resource kind.
pub const VAULT_ITEM_KIND: &str = "vault.item";
/// The vault's action that hands one item's secret to a process of an app's own, addressed at
/// the item ([`ItemRef::target`]).
pub const VAULT_RELEASE: &str = "release";
// The vault's actions crates published apart from their provider call by name; the provider's
// registry test checks each against its declaration.
/// The items Passwords lists.
pub const VAULT_LIST: &str = "list";
/// The item an app's own process kept for a purpose.
pub const VAULT_FIND: &str = "find";
/// A secret an app's own process keeps for a purpose.
pub const VAULT_KEEP: &str = "keep";
/// A key derived for this device and an app's purpose.
pub const VAULT_DEVICE_KEY: &str = "device-key";
/// The logins a page's app kept for a site.
pub const VAULT_PAGE_LOGINS: &str = "page-logins";
/// A login a page's app keeps for a site.
pub const VAULT_KEEP_PAGE_LOGIN: &str = "keep-page-login";
/// A login a page's app forgets for a site.
pub const VAULT_FORGET_PAGE_LOGIN: &str = "forget-page-login";
/// A login written into a page, addressed at the item ([`ItemRef::target`]).
pub const VAULT_PAGE_FILL: &str = "page-fill";
/// The website sign-ins the person brought, handed to Browser's window.
pub const VAULT_WEBSITE_SIGN_INS: &str = "website-sign-ins";
/// The vault's action that lists the person's website accounts, for Browser's window alone.
pub const VAULT_WEBSITE_ACCOUNTS: &str = "website-accounts";
/// The vault's action that opens one of the person's website accounts, for Browser's window
/// alone.
pub const VAULT_WEBSITE_ACCOUNT: &str = "website-account";

// The record kind the workspace keeps its vault in, in Passwords, where every action of the vault
// that writes takes its step, and the id its declaration binds to it.
pub use tilo_app_id::passwords::{VAULT_KIND, VAULT_RECORD};
/// The vault record's step that keeps an item, a new one or an edit of one.
pub const KEEP_SEALED: &str = "keep-sealed";
/// The vault record's step that forgets an item.
pub const REMOVE_SEALED: &str = "remove-sealed";
/// The vault record's step that writes down an item's uses.
pub const KEEP_USES: &str = "keep-uses";

/// The target of the vault's action `action` of [`VAULT_RESOURCE`].
#[must_use]
pub fn vault_target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: VAULT_PROVIDER.to_owned(),
        address: ActionAddress::new(VAULT_RESOURCE, action),
    }
}

/// A reference to one item of the workspace's vault: its address (`vault.item:<id>`), which
/// names it and is not its secret.
///
/// An agent may have one, pass it as an argument and keep it in a document. It opens nothing
/// by itself: the host resolves it only for whoever the workspace's vault lets reach the item,
/// and only at a place the item belongs to. The id is random, so a reference cannot be guessed
/// from another.
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
        let id = address.strip_prefix(VAULT_ITEM_KIND)?.strip_prefix(':')?;
        Self::from_id(id)
    }

    /// The id of the item it names.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The vault's action `action` addressed at this item (`vault.item:<id>#<action>`), so a
    /// grant on it is a grant on this one item alone.
    #[must_use]
    pub fn target(&self, action: &str) -> RemoteActionTarget {
        RemoteActionTarget {
            provider: VAULT_PROVIDER.to_owned(),
            address: ActionAddress::new(self.to_string(), action),
        }
    }

    /// The item an action at `target` of the vault is addressed at, when it is one of `action`
    /// and addressed at an item.
    #[must_use]
    pub fn addressed(target: &RemoteActionTarget, action: &str) -> Option<Self> {
        (target.provider == VAULT_PROVIDER && target.address.action == action)
            .then(|| Self::parse(&target.address.resource))
            .flatten()
    }
}

impl std::fmt::Display for ItemRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{VAULT_ITEM_KIND}:{}", self.id)
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

/// A parameter that takes a secret by reference: the address of an item of the workspace's
/// vault ([`ItemRef`]), never the secret's text.
///
/// It is a resource parameter of kind [`VAULT_ITEM_KIND`], so every surface that lists, checks
/// or records arguments carries it as it carries any record's address, and a draft shows it
/// withheld, as it shows a secret.
#[must_use]
pub fn item_param(name: &'static str, description: &'static str) -> crate::ActionParam {
    crate::ActionParam::required(name, crate::ParamType::Resource, description)
        .of_resource_kind(VAULT_ITEM_KIND)
}

/// The vault's actions whose answer is a secret ([`crate::SECRET_ANSWER_TAG`]): a secret shown
/// to its person, and the code of this moment.
pub const VAULT_SECRET_ANSWERS: [&str; 2] = ["reveal", "code"];

/// Whether `param` takes a secret by reference ([`item_param`]).
#[must_use]
pub fn takes_item(param: &crate::ActionParam) -> bool {
    param.ty == crate::ParamType::Resource && param.resource_kind() == Some(VAULT_ITEM_KIND)
}

/// Whether `request` answers a secret, so that whatever records it keeps neither its answer nor
/// its arguments: an action whose declaration carries
/// [`SECRET_ANSWER_TAG`](crate::SECRET_ANSWER_TAG), or a call of the action registry that names
/// one by address.
#[must_use]
pub fn answers_secret(request: &InvokeRequest) -> bool {
    super::answers_a_secret(&request.target.address.action) || calls_answering_a_secret(request)
}

/// The address a call of the action registry (`tilo.actions#call`) names, when `request` is one.
fn called_address(request: &InvokeRequest) -> Option<&str> {
    let target = &request.target;
    if target.address.action != "call" || target.provider != super::ACTIONS_PROVIDER {
        return None;
    }
    match &request.argument {
        InvokeArgument::Json(arguments) => {
            arguments.get("address").and_then(serde_json::Value::as_str)
        }
        InvokeArgument::None | InvokeArgument::Text(_) => None,
    }
}

/// Whether `request` is a call of the action registry (`tilo.actions#call`) that names, by its
/// address, an action whose answer is a secret.
#[must_use]
pub fn calls_answering_a_secret(request: &InvokeRequest) -> bool {
    called_address(request)
        .and_then(|address| address.rsplit_once('#'))
        .is_some_and(|(_, action)| super::answers_a_secret(action))
}

/// Whether `request` is a call of the action registry (`tilo.actions#call`) that names the
/// vault's `action` by its address (`tilo.vault/vault#<action>`, or the vault in a workspace's
/// tree): what Passwords' code makes of the vault, whose record follows the vault's own.
#[must_use]
pub fn calls_by_address(request: &InvokeRequest, action: &str) -> bool {
    called_address(request)
        .and_then(|address| address.strip_suffix(action))
        .and_then(|rest| rest.strip_suffix('#'))
        .is_some_and(|rest| rest.ends_with(super::WORKSPACE_VAULT))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
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
        let release = reference.target(VAULT_RELEASE);
        assert_eq!(release.address.resource, format!("vault.item:{ID}"));
        assert_eq!(ItemRef::addressed(&release, VAULT_RELEASE), Some(reference));
        assert_eq!(ItemRef::addressed(&release, "code"), None);

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

    /// A parameter that takes a secret by reference is a resource parameter of the vault
    /// item's kind, and what binds as a reference there is an item's address, never typed
    /// text.
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
        let reference = |args: crate::ActionArgs| match args.get("password") {
            Some(crate::ArgValue::Resource(address) | crate::ArgValue::Text(address)) => {
                ItemRef::parse(address)
            }
            _ => None,
        };
        let bound = crate::ActionArgs::bind_json(&params, named.as_object().unwrap()).unwrap();
        assert_eq!(reference(bound), ItemRef::from_id(ID));
        let typed = serde_json::json!({"password": "hunter2"});
        let bound = crate::ActionArgs::bind_json(&params, typed.as_object().unwrap());
        // Text that is no address either does not bind or binds as no reference.
        assert!(!bound.is_ok_and(|bound| reference(bound).is_some()));
    }
}

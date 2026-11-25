//! Which items of the workspace's vault the workspace's Linux machine hands its programs, and
//! under which variable: one workspace document Terminal keeps, its members change it with
//! [`MachineSecrets::ADD_SECRET`] and [`MachineSecrets::REMOVE_SECRET`] as the workspace's
//! policy lets them.
//!
//! The document names each item by its reference (`vault.item:<id>`), never its secret. The
//! server reads it when a command, a terminal or another program starts in the machine, has each
//! item released from the vault for whoever started it, and puts the values only in the
//! environment of the programs it starts there.

use tilo_resource::{JsonSchema, Model, resource};

/// The kind this document's record is of.
pub const KIND: &str = "terminal.machine-secrets";

/// The workspace's one document of them, in Terminal: a workspace document of Terminal's
/// (`collab_types::WORKSPACE_DOCUMENTS`), named by its kind so every device and its server
/// derive the same id and opening it needs no window.
pub const DOCUMENT: &str = "derived:terminal.machine-secrets@tab:0000000000000000";

/// What the address of a vault item begins with.
const ITEM_PREFIX: &str = "vault.item:";

/// Variables a program in the machine starts with that no secret may take the place of.
const KEPT_VARIABLES: &[&str] = &[
    "HOME", "PATH", "USER", "LOGNAME", "SHELL", "TERM", "PWD", "LANG", "TMPDIR",
];

/// The beginnings of variables the machine's own software reads.
const KEPT_PREFIXES: &[&str] = &["LD_", "LC_", "USH_", "TILO_", "XDG_"];

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct MachineSecrets {
    /// Oldest first, one per variable.
    pub secrets: Vec<MachineSecret>,
}

/// One item and the variable its secret arrives in.
#[derive(
    Model, Default, Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema,
)]
pub struct MachineSecret {
    /// The variable, such as `GITHUB_TOKEN`.
    pub variable: String,
    /// The vault item's reference, `vault.item:<id>`.
    pub item: String,
}

#[resource(
    kind = "terminal.machine-secrets",
    read_content = tilo_resource::read::fields
)]
impl MachineSecrets {
    /// Add secret.
    ///
    /// Hands the vault item `item` to every program the workspace's machine starts, in
    /// `variable`. Naming a variable again replaces its item.
    #[action]
    pub fn add_secret(
        &mut self,
        #[param("Variable", example = "GITHUB_TOKEN")] variable: String,
        #[param(
            "Vault item",
            resource = "vault.item",
            example = "vault.item:0123456789abcdef0123456789abcdef"
        )]
        item: String,
    ) -> Result<(), String> {
        let variable = variable.trim();
        check_variable(variable)?;
        let item = item.trim();
        if !is_item(item) {
            return Err("a vault item is named by its reference, vault.item:<id>".into());
        }
        let secret = MachineSecret {
            variable: variable.to_owned(),
            item: item.to_owned(),
        };
        match self
            .secrets
            .iter_mut()
            .find(|held| held.variable == variable)
        {
            Some(held) => *held = secret,
            None => self.secrets.push(secret),
        }
        Ok(())
    }

    /// Remove secret.
    ///
    /// The machine's programs started from now on no longer get `variable`.
    #[action(destructive)]
    pub fn remove_secret(&mut self, #[param("Variable")] variable: String) -> Result<(), String> {
        let variable = variable.trim();
        let before = self.secrets.len();
        self.secrets.retain(|held| held.variable != variable);
        if self.secrets.len() == before {
            return Err(format!("no secret is handed to the machine in {variable}"));
        }
        Ok(())
    }
}

/// Whether `variable` may carry a secret: a name a shell takes (`A-Z`, `0-9` and `_`, not
/// starting with a digit), and none the machine's own software reads.
fn check_variable(variable: &str) -> Result<(), String> {
    let mut chars = variable.chars();
    let named = chars
        .next()
        .is_some_and(|first| first.is_ascii_uppercase() || first == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !named {
        return Err(format!(
            "{variable:?} is no variable name: capital letters, digits and _, not starting with a digit"
        ));
    }
    if KEPT_VARIABLES.contains(&variable)
        || KEPT_PREFIXES
            .iter()
            .any(|prefix| variable.starts_with(prefix))
    {
        return Err(format!(
            "{variable} is the machine's own; a secret goes in a variable of its own"
        ));
    }
    Ok(())
}

/// Whether `item` is a vault item's reference: [`ITEM_PREFIX`] and 32 lowercase hexadecimal
/// digits, as the vault spells them.
fn is_item(item: &str) -> bool {
    item.strip_prefix(ITEM_PREFIX).is_some_and(|id| {
        id.len() == 32
            && id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource::AppNamed;
    use tilo_resource::Replica;

    const ITEM: &str = "vault.item:0123456789abcdef0123456789abcdef";

    #[test]
    fn the_document_is_terminals_workspace_document_of_this_kind() {
        assert_eq!(DOCUMENT, collab_types::workspace_document_id(KIND));
        assert_eq!(
            collab_types::workspace_document("so.tilo.terminal", DOCUMENT).map(|kept| kept.kind),
            Some(KIND),
            "every device and its server name the one document by its kind"
        );
    }

    fn replica() -> Replica<MachineSecrets, AppNamed> {
        let mut doc = tilo_resource::Doc::new();
        tilo_resource::Fields::write_fields(&MachineSecrets::default(), &mut doc, &automerge::ROOT)
            .unwrap();
        doc.commit();
        Replica::from_doc(doc).in_app("so.tilo.model", "Model")
    }

    fn add(variable: &str, item: &str) -> tilo_resource::Invoke<MachineSecrets> {
        MachineSecrets::ADD_SECRET.with((variable.to_owned(), item.to_owned()))
    }

    #[test]
    fn the_item_is_a_vault_resource_a_person_picks() {
        let declarations = <MachineSecrets as tilo_resource::Resource>::declarations();
        let declaration = declarations
            .iter()
            .find(|declaration| declaration.id == "add-secret")
            .expect("Terminal offers the action");
        let item = declaration
            .params
            .iter()
            .find(|param| param.name.as_ref() == "item")
            .expect("it takes an item");
        assert_eq!(
            item.resource_kind(),
            Some("vault.item"),
            "the item is picked from the workspace's vault, never typed"
        );
    }

    #[test]
    fn a_variable_takes_one_item_and_is_removed_whole() {
        let mut r = replica();
        r.apply(add("GITHUB_TOKEN", ITEM)).unwrap();
        let other = "vault.item:ffffffffffffffffffffffffffffffff";
        r.apply(add("GITHUB_TOKEN", other)).unwrap();
        assert_eq!(
            r.state().unwrap().secrets,
            vec![MachineSecret {
                variable: "GITHUB_TOKEN".into(),
                item: other.into(),
            }]
        );
        r.apply(MachineSecrets::REMOVE_SECRET.with("GITHUB_TOKEN".to_owned()))
            .unwrap();
        assert!(r.state().unwrap().secrets.is_empty());
        assert!(
            r.apply(MachineSecrets::REMOVE_SECRET.with("GITHUB_TOKEN".to_owned()))
                .is_err()
        );
    }

    #[test]
    fn only_a_reference_goes_in_and_never_over_the_machines_own_variables() {
        let mut r = replica();
        for (variable, item) in [
            ("TOKEN", "the-secret-itself"),
            ("TOKEN", "vault.item:0123"),
            ("PATH", ITEM),
            ("LD_PRELOAD", ITEM),
            ("TILO_ANYTHING", ITEM),
            ("lower", ITEM),
            ("9LIVES", ITEM),
            ("A=B", ITEM),
        ] {
            assert!(r.apply(add(variable, item)).is_err(), "{variable} {item}");
        }
        assert!(r.state().unwrap().secrets.is_empty());
    }
}

//! The workspace's vault, as this service reads and writes it: Passwords' document of sealed
//! entries, which the service holds the keys of as a device that keeps the workspace does. The
//! entries are the document's own: each sealed under its keys, with a use kept beside it.

use tilo_resource::{self, Atom, Collection, Id, Model, Refusal, Set, resource};

/// The workspace's vault: the one place a workspace keeps what its members and agents sign in
/// with. Each entry is sealed under the document's own keys, which every device policy lets
/// read Passwords holds, a server among them; a reader without them reads nothing of an entry.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct WorkspaceVaultEntries {
    #[model(readonly)]
    pub sealed: Collection<Atom>,
    #[model(readonly)]
    pub imported: Collection<Atom>,
    #[model(readonly)]
    pub removed: Set<Id>,
    /// When, where and for whom each entry was handed to something that signs in, sealed per
    /// entry beside it ([`USES_FIELD`]): a use is written down here, so recording one never
    /// rewrites the entry a member may be editing at the same time.
    #[model(readonly)]
    pub used: Collection<Atom>,
}

/// The sealed field an entry's uses are kept at, beside the entry `id` itself.
#[must_use]
pub fn uses_field(id: &str) -> String {
    format!("{id}{USES_FIELD}")
}

/// What [`uses_field`] adds to an entry's id.
pub const USES_FIELD: &str = "/uses";

impl WorkspaceVaultEntries {
    /// An explicit workspace write takes precedence over an imported value with the same id.
    pub fn entry(&self, id: &Id) -> Option<&Atom> {
        self.sealed.get(id).or_else(|| {
            if self.removed.contains(id) {
                None
            } else {
                self.imported.get(id)
            }
        })
    }

    /// The entries currently present in the vault.
    pub fn entries(&self) -> impl Iterator<Item = (&Id, &Atom)> {
        self.sealed.iter().chain(
            self.imported
                .iter()
                .filter(|(id, _)| !self.sealed.contains(id) && !self.removed.contains(id)),
        )
    }

    /// The semantic read exposes entries through the live vault, where each read can
    /// check the current keys.
    pub fn read_document(&self, _: Option<&Id>) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({}))
    }

    fn validate_sealed(id: &Id, ciphertext: &str) -> Result<(), Refusal> {
        const MAX_HEX_BYTES: usize = 2 * 1024 * 1024;
        if id.as_str().is_empty()
            || id.as_str().len() > 128
            || ciphertext.is_empty()
            || ciphertext.len() > MAX_HEX_BYTES
            || !ciphertext.len().is_multiple_of(2)
            || !ciphertext.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(Refusal::new(
                "a vault entry needs a valid id and ciphertext",
            ));
        }
        Ok(())
    }
}

#[resource(
    kind = "passwords.workspace-vault",
    read_content = WorkspaceVaultEntries::read_document
)]
impl WorkspaceVaultEntries {
    /// Save sealed vault entry
    ///
    /// The vault window seals the item and its metadata under this document's keys.
    #[action(writes = sealed)]
    pub fn keep_sealed(
        &mut self,
        #[param("Opaque item identifier")] id: String,
        #[param("Encrypted entry")] ciphertext: String,
    ) -> Result<(), Refusal> {
        let id = Id::from(id);
        Self::validate_sealed(&id, &ciphertext)?;
        if let Some(current) = self.sealed.get_mut(&id) {
            *current = Atom(ciphertext);
        } else {
            self.sealed.push(id, Atom(ciphertext));
        }
        Ok(())
    }

    /// Keep an entry from an older vault
    ///
    /// Each identifier is imported once. An existing entry takes precedence and a
    /// repeat import does not bring back an entry the workspace subsequently deleted.
    #[action(writes = imported)]
    pub fn import_sealed(
        &mut self,
        #[param("Opaque item identifier")] id: String,
        #[param("Encrypted entry")] ciphertext: String,
    ) -> Result<(), Refusal> {
        let id = Id::from(id);
        Self::validate_sealed(&id, &ciphertext)?;
        if !self.removed.contains(&id) && self.entry(&id).is_none() {
            self.imported.push(id, Atom(ciphertext));
        }
        Ok(())
    }

    /// Delete sealed vault entry
    #[action(destructive, writes = sealed, writes = imported, writes = removed, writes = used)]
    pub fn remove_sealed(&mut self, #[param("Opaque item identifier")] id: String) {
        let id = Id::from(id);
        self.sealed.remove(&id);
        self.imported.remove(&id);
        self.used.remove(&id);
        self.removed.insert(id);
    }

    /// Record where an entry was used
    ///
    /// The uses are sealed under this document's keys at the entry's uses field. An entry that
    /// is gone keeps none.
    #[action(writes = used)]
    pub fn keep_uses(
        &mut self,
        #[param("Opaque item identifier")] id: String,
        #[param("Encrypted uses")] ciphertext: String,
    ) -> Result<(), Refusal> {
        let id = Id::from(id);
        Self::validate_sealed(&id, &ciphertext)?;
        if self.entry(&id).is_none() {
            return Err(Refusal::new("that entry is not in the vault"));
        }
        if let Some(current) = self.used.get_mut(&id) {
            *current = Atom(ciphertext);
        } else {
            self.used.push(id, Atom(ciphertext));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource::Replica;

    #[test]
    fn an_import_preserves_workspace_edits_and_deletions() {
        let key = "login".to_owned();
        let id = Id::from(key.clone());
        let mut vault = Replica::<WorkspaceVaultEntries>::new();
        vault
            .apply(WorkspaceVaultEntries::IMPORT_SEALED.with((key.clone(), "ab".to_owned())))
            .unwrap();
        vault
            .apply(WorkspaceVaultEntries::KEEP_SEALED.with((key.clone(), "cd".to_owned())))
            .unwrap();
        vault
            .apply(WorkspaceVaultEntries::IMPORT_SEALED.with((key.clone(), "ef".to_owned())))
            .unwrap();
        assert_eq!(
            vault.state().unwrap().entry(&id),
            Some(&Atom("cd".to_owned()))
        );
        vault
            .apply_confirmed(WorkspaceVaultEntries::REMOVE_SEALED.with(key.clone()))
            .unwrap();
        vault
            .apply(WorkspaceVaultEntries::IMPORT_SEALED.with((key, "ef".to_owned())))
            .unwrap();
        assert!(vault.state().unwrap().entry(&id).is_none());
    }

    #[test]
    fn a_workspace_write_wins_over_a_concurrent_import() {
        let key = "login".to_owned();
        let id = Id::from(key.clone());
        let mut workspace = Replica::<WorkspaceVaultEntries>::new();
        let mut importer = workspace.fork();
        workspace
            .apply(WorkspaceVaultEntries::KEEP_SEALED.with((key.clone(), "ab".to_owned())))
            .unwrap();
        importer
            .apply(WorkspaceVaultEntries::IMPORT_SEALED.with((key, "cd".to_owned())))
            .unwrap();
        workspace.sync_with(&mut importer).unwrap();
        assert_eq!(
            workspace.state().unwrap().entry(&id),
            Some(&Atom("ab".to_owned()))
        );
        assert_eq!(
            importer.state().unwrap().entry(&id),
            Some(&Atom("ab".to_owned()))
        );
    }
}

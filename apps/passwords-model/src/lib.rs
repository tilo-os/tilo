//! The Passwords resource carries encrypted workspace vault entries and their opaque ids.

use tilo_resource::{self, Atom, Collection, Id, Model, Refusal, Set, resource};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Passwords {}

impl Passwords {
    /// What the vault holds: nothing of its own. Its entries are the sealed
    /// document beside it, which a reader opens under its keys; a caller with
    /// no window open still learns that the vault is there.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(kind = "passwords.vault", read_content = Passwords::read_document)]
impl Passwords {}

/// The vault's published document. The reader presents decrypted entries only while it has
/// the independent document keys. Legacy documents have no entries.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct WorkspaceVaultEntries {
    #[model(readonly)]
    pub sealed: Collection<Atom>,
    #[model(readonly)]
    pub imported: Collection<Atom>,
    #[model(readonly)]
    pub removed: Set<Id>,
}

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
    #[action(destructive, writes = sealed, writes = imported, writes = removed)]
    pub fn remove_sealed(&mut self, #[param("Opaque item identifier")] id: String) {
        let id = Id::from(id);
        self.sealed.remove(&id);
        self.imported.remove(&id);
        self.removed.insert(id);
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

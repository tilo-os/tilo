//! Where a record lives: the id of a person's record of a kind, of a chunk of it, and of a
//! record that remembers the workspace its contents refer to.
//!
//! A record's id is derived, never assigned: the same on every device, so two devices agree on
//! where a document lives without naming each other. A workspace's document of a kind has the
//! same id ([`crate::workspace_document::workspace_document_id`]).
//!
//! [`personal_cluster`] is the cluster the retained store's records are routed under: a copy of
//! a person's records a remote install left on its disk, read once at start-up and written by
//! nothing. A record kept now is a resource of the workspace that keeps it, on that
//! workspace's own cluster.

use sha2::{Digest as _, Sha256};

/// Domain separation for the derivation, so this id cannot collide with
/// another hash of the same key.
const DOMAIN: &[u8] = b"tilo-personal-store-v1\0";

/// The cluster the retained store's records of the person whose root public key is `root` are
/// routed under: a version-8 uuid every device of that person derives alike, without asking
/// anyone.
#[must_use]
pub fn personal_cluster(root: &[u8; 32]) -> uuid::Uuid {
    let digest = Sha256::new()
        .chain_update(DOMAIN)
        .chain_update(root)
        .finalize();
    let mut bytes = [0u8; 16];
    for (byte, digested) in bytes.iter_mut().zip(digest) {
        *byte = digested;
    }
    uuid::Builder::from_custom_bytes(bytes).into_uuid()
}

// The id of the person's record of `kind`: one per kind, the same on every device, kept in their
// own workspace and derived from the app's base record.
pub use app_fleet::derived_record as person_document_id;

/// A personal record that remembers which workspace its contents refer to.
#[must_use]
pub fn person_document_for_workspace(kind: &str, workspace: uuid::Uuid) -> String {
    format!("derived:context.{workspace}@{}", person_document_id(kind))
}

/// The workspace context recorded in a personal document's id.
#[must_use]
pub fn person_document_context(record: &str) -> Option<uuid::Uuid> {
    let (context, _) = profile_context(chunk_base(record))?;
    Some(context)
}

fn profile_context(record: &str) -> Option<(uuid::Uuid, &str)> {
    let (context, base) = record.strip_prefix("derived:context.")?.split_once('@')?;
    let id = uuid::Uuid::parse_str(context).ok()?;
    (id.to_string() == context && app_fleet::derived_kind(base).is_some()).then_some((id, base))
}

/// The kind a person record's id names; `None` for any other record. A chunk of one
/// (`derived:chunk.<n>@<record>`, `n` from 1) is part of the record it splits, and names
/// the same kind.
#[must_use]
pub fn person_resource_type(record: &str) -> Option<&str> {
    let record = chunk_base(record);
    let record = profile_context(record).map_or(record, |(_, base)| base);
    app_fleet::derived_kind(record)
}

/// The record `record` is a chunk of, for a chunk record id
/// (`derived:chunk.<n>@<record>`, `n` from 1); `record` itself otherwise.
fn chunk_base(record: &str) -> &str {
    let Some(derived) = record.strip_prefix("derived:") else {
        return record;
    };
    let Some((role, base)) = derived.split_once('@') else {
        return record;
    };
    let Some(number) = role.strip_prefix("chunk.") else {
        return record;
    };
    let canonical = !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && !number.starts_with('0');
    if canonical { base } else { record }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        /// The record a person keeps for themselves: its kind and the store every device of theirs derives.
        fn a_persons_record_and_store_are_their_own;

        phase a_person_record_names_its_kind;
        {
            let record = person_document_id("terminal.history");
            assert_eq!(record, "derived:terminal.history@tab:0000000000000000");
            assert_eq!(person_resource_type(&record), Some("terminal.history"));
            for other in [
                "tab:000000000000005c",
                "tab:0000000000000000",
                "derived:a@b@tab:0000000000000000",
                "derived:terminal.history@derived:default@tab:0000000000000000",
                "derived:notes@tab:000000000000005c",
            ] {
                assert_eq!(person_resource_type(other), None, "{other}");
            }
        }

        phase a_chunk_of_a_persons_record_belongs_to_the_record_it_splits;
        // A record the platform splits into chunks keeps them as records of their own
        // (`derived:chunk.<n>@<record>`, `n` from 1): the record's own is chunk 0 and
        // names the record's kind, and so does each chunk of it.
        {
            let tree = person_document_id("files.tree");
            for chunk in [1, 2, 77] {
                let record = format!("derived:chunk.{chunk}@{tree}");
                assert_eq!(
                    person_resource_type(&record),
                    Some("files.tree"),
                    "{record} names its record's kind"
                );
            }
            assert_eq!(person_resource_type(&tree), Some("files.tree"), "chunk 0 is the record");
            for other in [
                format!("derived:chunk.01@{tree}"),
                format!("derived:chunk.@{tree}"),
                "derived:chunk.1@derived:chunk.2@derived:files.tree@tab:0000000000000000".to_owned(),
                "derived:chunk.1@nonsense".to_owned(),
            ] {
                assert_eq!(person_resource_type(&other), None, "{other}");
            }
        }

        phase a_person_has_one_retained_cluster_every_device_derives_and_nobody_else_shares;
        // Every device of a person derives one cluster for their retained store, and nobody
        // else's derives it.
        {
            let ada = [7u8; 32];
            let bob = [8u8; 32];
            let cluster = personal_cluster(&ada);
            assert_eq!(
                cluster,
                personal_cluster(&ada),
                "every device derives the same cluster"
            );
            assert_ne!(
                cluster,
                personal_cluster(&bob),
                "another person has another retained store"
            );
            assert!(!cluster.is_nil());
            assert_eq!(cluster.get_version_num(), 8);
        }
    }
}

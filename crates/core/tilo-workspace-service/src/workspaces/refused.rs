//! The records of the content a host stopped keeping, as documents of the
//! workspace ([`collab_types::CONTENT_REFUSAL_APP`]): one per refused content,
//! keyed by the record that named it and the content's address
//! ([`collab_types::content_refusal_document_id`]), so a host that restarts
//! finds the refusal it made and never refuses the same content twice.
//!
//! A refusal is written as the host's own record, signed and sealed as the
//! device, and every device of the workspace takes it as it takes any record's
//! write. Its fields are the refusal a person reads: the record whose content
//! it is, the content's address and size, the limit it went over and what that
//! allowed, who posted it, when it was refused, who can raise the limit, and
//! who the refusal is for. A refusal whose limit was raised and whose content
//! the host kept is written again with `withdrawn` set: the record stays, as a
//! version history does, and no reader shows it as standing.

use std::collections::BTreeMap;

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc as _, ScalarValue, Value as AmValue};

use tilo_action_policy::{ContentLimit, Raises};
use tilo_actions::{Actor, MemberId};

use super::Workspaces;

/// The kind of a refusal record: what [`collab_types::CONTENT_REFUSAL_KIND`]
/// names, and what the record's `kind` holds.
pub const KIND: &str = collab_types::CONTENT_REFUSAL_KIND;

/// The app the host writes its refusal records under.
pub const APP: &str = collab_types::CONTENT_REFUSAL_APP;

/// One record of the content a host refused: the fields a person reads, and
/// the people the record names as its readers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// The address of the record whose content it is, `app/document`.
    pub record: String,
    /// The content's address, lowercase BLAKE3 hex.
    pub hash: String,
    pub bytes: u64,
    pub limit: ContentLimit,
    pub allowed: u64,
    /// The member who posted it, as the workspace knows them.
    pub member: Actor,
    /// When it was refused, milliseconds since the Unix epoch.
    pub at: u64,
    pub raise: Raises,
    /// The people the refusal is for: the poster, the workspace's owners and
    /// the host's person, by their root keys.
    pub readers: Vec<[u8; 32]>,
    /// The limit was raised and the content is kept: the record no longer
    /// stands.
    pub withdrawn: bool,
}

fn now_ms() -> u64 {
    tilo_clock::unix_now_ns() / 1_000_000
}

/// `actor` as a person reads it: a member by their person id, an automation or
/// an app by its id, a visitor by their key. A refusal names its poster this
/// way, so a reader compares the id with its own.
fn actor_text(actor: &Actor) -> String {
    match actor {
        Actor::Member(member) => member.person_id().map(str::to_owned).unwrap_or_default(),
        Actor::Automation(id) => id.as_str().to_owned(),
        Actor::App(app) => app.to_string(),
        Actor::Visitor(key) => key.clone(),
    }
}

impl Refusal {
    /// The id of this refusal's document in the workspace.
    #[must_use]
    pub fn id(&self) -> String {
        let (app, record) = self.record.split_once('/').unwrap_or(("", &self.record));
        collab_types::content_refusal_document_id(app, record, &self.hash)
    }

    /// The refusal a person reads, without the people it is kept for.
    #[must_use]
    pub fn read(&self) -> super::ContentRefusal {
        super::ContentRefusal {
            record: self.record.clone(),
            hash: self.hash.clone(),
            bytes: self.bytes,
            limit: self.limit,
            allowed: self.allowed,
            member: actor_text(&self.member),
            at: self.at,
            raise: self.raise,
        }
    }

    /// The changes that write this refusal into a record `held` already, or
    /// the whole record it founds, and whether it founds one. The change names
    /// the host's person as the one who made it, as every record's does: a
    /// change that names no invoker is refused by the workspace.
    fn write(&self, held: Option<&[u8]>, actor: &Actor) -> Result<(Vec<u8>, bool), String> {
        let (mut document, founding) = match held {
            Some(bytes) => (
                AutoCommit::load(bytes).map_err(|error| error.to_string())?,
                false,
            ),
            None => (AutoCommit::new(), true),
        };
        let heads = document.get_heads();
        let failing = |error: automerge::AutomergeError| error.to_string();
        document.put(ROOT, "kind", KIND).map_err(failing)?;
        document.put(ROOT, "record", self.record.as_str()).map_err(failing)?;
        document.put(ROOT, "hash", self.hash.as_str()).map_err(failing)?;
        document.put(ROOT, "bytes", i64::try_from(self.bytes).unwrap_or(i64::MAX))
            .map_err(failing)?;
        document.put(ROOT, "limit", self.limit.key()).map_err(failing)?;
        document.put(
            ROOT,
            "allowed",
            i64::try_from(self.allowed).unwrap_or(i64::MAX),
        )
        .map_err(failing)?;
        document.put(
            ROOT,
            "member",
            serde_json::to_string(&self.member).unwrap_or_default(),
        )
        .map_err(failing)?;
        document.put(ROOT, "at", i64::try_from(self.at).unwrap_or(i64::MAX))
            .map_err(failing)?;
        document.put(ROOT, "raise", self.raise.key()).map_err(failing)?;
        document.put(ROOT, "withdrawn", self.withdrawn).map_err(failing)?;
        if founding {
            let readers = document
                .put_object(ROOT, "readers", ObjType::List)
                .map_err(failing)?;
            for (at, person) in self.readers.iter().enumerate() {
                let reader = Actor::Member(MemberId::person(
                    tilo_actions::PersonId::from_root(*person),
                ));
                document.insert(&readers, at, serde_json::to_string(&reader).unwrap_or_default())
                    .map_err(failing)?;
            }
        }
        let stamp = tilo_resource_store::Stamp {
            verb: "content.refused".to_owned(),
            invoker: tilo_resource_store::Stamp::by(actor),
            invocation: format!("refusal:{}", self.id()),
            ..tilo_resource_store::Stamp::default()
        };
        let committed = document.commit_with(
            CommitOptions::default()
                .with_message(stamp.message())
                .with_time(i64::try_from(now_ms()).unwrap_or(i64::MAX)),
        );
        if committed.is_none() {
            return Ok((Vec::new(), founding));
        }
        Ok((
            if founding {
                document.save()
            } else {
                document.save_after(&heads)
            },
            founding,
        ))
    }

    /// The refusal a record's bytes hold, or `None` when they hold none.
    fn of_document(bytes: &[u8]) -> Option<Self> {
        let document = AutoCommit::load(bytes).ok()?;
        let text = |name: &str| match document.get(ROOT, name).ok().flatten() {
            Some((AmValue::Scalar(scalar), _)) => match scalar.as_ref() {
                ScalarValue::Str(text) => Some(text.to_string()),
                _ => None,
            },
            _ => None,
        };
        let number = |name: &str| match document.get(ROOT, name).ok().flatten() {
            Some((AmValue::Scalar(scalar), _)) => match scalar.as_ref() {
                ScalarValue::Int(number) | ScalarValue::Timestamp(number) => {
                    u64::try_from(*number).ok()
                }
                ScalarValue::Uint(number) => Some(*number),
                _ => None,
            },
            _ => None,
        };
        let flag = |name: &str| {
            document.get(ROOT, name).ok().flatten().is_some_and(
                |(value, _)| matches!(value, AmValue::Scalar(scalar) if scalar.as_ref() == &ScalarValue::Boolean(true)),
            )
        };
        let readers = match document.get(ROOT, "readers").ok().flatten() {
            Some((AmValue::Object(ObjType::List), list)) => document
                .list_range(&list, ..)
                .filter_map(|item| match item.value {
                    automerge::ValueRef::Scalar(automerge::ScalarValueRef::Str(text)) => {
                        serde_json::from_str::<Actor>(&text).ok()
                    }
                    _ => None,
                })
                .filter_map(|actor| actor.member_id()?.person_id().map(str::to_owned))
                .filter_map(|person| {
                    let mut root = [0_u8; 32];
                    hex::decode_to_slice(&person, &mut root).ok().map(|()| root)
                })
                .collect(),
            _ => Vec::new(),
        };
        Some(Self {
            record: text("record")?,
            hash: text("hash")?,
            bytes: number("bytes")?,
            limit: ContentLimit::parse(&text("limit")?)?,
            allowed: number("allowed")?,
            member: serde_json::from_str(&text("member")?).ok()?,
            at: number("at")?,
            raise: Raises::parse(&text("raise")?)?,
            readers,
            withdrawn: flag("withdrawn"),
        })
    }
}

impl Workspaces {
    /// Record `refusal` in `workspace`, as this device: the one record of the
    /// content `refusal.hash` of its record went over a limit for. Written
    /// again for a refusal whose limit was raised and whose content was kept,
    /// the record says so and no reader shows it standing.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, the record does not read, or the
    /// workspace refused the write.
    pub async fn record_refusal(&self, workspace: &str, refusal: &Refusal) -> Result<(), String> {
        let id = refusal.id();
        let held = self
            .resource_document(workspace, APP, &id)
            .await
            .map_err(|error| error.to_string())?;
        let host = Actor::Member(MemberId::person(tilo_actions::PersonId::from_root(
            self.device_person(),
        )));
        let (changes, founding) = refusal.write(held.as_deref(), &host)?;
        if changes.is_empty() {
            return Ok(());
        }
        self.own_write(
            workspace,
            (APP, &id),
            None,
            &changes,
            founding,
            founding,
            None,
        )
        .await
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Every refusal standing in `workspace`, read back from its records: what
    /// its members read as the content this host stopped keeping.
    pub async fn refusals(&self, workspace: &str) -> Vec<Refusal> {
        let Ok(routes) = self.workspace_document_routes(workspace).await else {
            return Vec::new();
        };
        let mut standing = BTreeMap::new();
        for (app, document) in routes {
            if !collab_types::is_content_refusal_document(&document) {
                continue;
            }
            let Ok(Some(bytes)) = self.resource_document(workspace, &app, &document).await else {
                continue;
            };
            if let Some(refusal) = Refusal::of_document(&bytes)
                && !refusal.withdrawn
            {
                standing.insert(document, refusal);
            }
        }
        standing.into_values().collect()
    }

    /// Every refusal standing in `workspace` that `reader` reads: the content
    /// they posted, the workspace's owners', and the host's person's. A
    /// member reads none of another member's refusals.
    pub async fn refusals_for(&self, workspace: &str, reader: [u8; 32]) -> Vec<Refusal> {
        self.refusals(workspace)
            .await
            .into_iter()
            .filter(|refusal| refusal.readers.contains(&reader))
            .collect()
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::workspaces::fixtures::{governor, test_host};
    use tilo_actions::PersonId;

    /// A refusal is a record of the workspace: the host writes it as its own
    /// write, a device that opens the workspace again reads it back, and once
    /// its limit is raised and the content kept, the record says it no longer
    /// stands.
    #[tokio::test]
    async fn a_refusal_is_a_record_of_the_workspace_and_reads_back_from_it() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let summary = workspaces.create("Lobby").await.unwrap();
        let workspace = summary.workspace.clone();
        let poster = [3_u8; 32];
        let refusal = Refusal {
            record: crate::workspaces::ContentRefusal::address(tilo_app_id::apps::CHAT, ""),
            hash: "ab".repeat(32),
            bytes: 800,
            limit: ContentLimit::Member,
            allowed: 1_500,
            member: Actor::Member(MemberId::person(PersonId::from_root(poster))),
            at: 7,
            raise: Raises::Owners,
            readers: vec![owner.user_public_key, poster],
            withdrawn: false,
        };
        workspaces
            .record_refusal(&workspace, &refusal)
            .await
            .expect("the refusal is recorded in the workspace");
        assert_eq!(
            workspaces.refusals(&workspace).await,
            vec![refusal.clone()],
            "the record reads back from the workspace"
        );
        assert_eq!(
            workspaces.refusals_for(&workspace, poster).await,
            vec![refusal.clone()],
            "the poster reads their own refusal"
        );
        assert_eq!(
            workspaces
                .refusals_for(&workspace, owner.user_public_key)
                .await,
            vec![refusal.clone()],
            "and the workspace's owner"
        );
        assert!(
            workspaces
                .refusals_for(&workspace, [9_u8; 32])
                .await
                .is_empty(),
            "a member who posted none of it reads none of it"
        );

        let mut withdrawn = refusal.clone();
        withdrawn.withdrawn = true;
        workspaces
            .record_refusal(&workspace, &withdrawn)
            .await
            .expect("the withdrawal lands on the same record");
        assert!(
            workspaces.refusals(&workspace).await.is_empty(),
            "a withdrawn refusal stands for no reader"
        );
        server.shutdown().await;
    }
}

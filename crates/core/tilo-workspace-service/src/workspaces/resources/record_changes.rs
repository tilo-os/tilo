//! The record an app's process opened, as this device keeps it: the changes the process merges
//! into its own copy, and the changes it makes, which this device writes as its own. The process
//! holds no key and signs nothing; the broker decided the caller's read or write already.

use automerge::{ChangeHash, ReadDoc as _};
use tilo_resource_store::wire::{Bytes, RECORD_PAGE_BYTES, Reply};

use super::super::Workspaces;

impl Workspaces {
    /// The page from byte `from` of the changes of `app`'s `record` in `workspace` past the heads
    /// `after` (hex), in the clear, and the heads they bring it to: none of a record this device
    /// holds nothing of. A head the record lacks is one of the app's own changes it has not
    /// handed over yet, and says nothing of what it holds besides.
    pub(in crate::workspaces) async fn opened_record_changes(
        &self,
        workspace: &str,
        (app, record): (&str, &str),
        after: &[String],
        from: u64,
    ) -> Result<Reply, String> {
        let after = after
            .iter()
            .map(|head| {
                head.parse::<ChangeHash>()
                    .map_err(|_| format!("{head} names no change of a record"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let Some(bytes) = self
            .resource_document(workspace, app, record)
            .await
            .map_err(|error| error.to_string())?
        else {
            return Ok(Reply::RecordChanges {
                changes: Bytes::default(),
                heads: Vec::new(),
                next: None,
            });
        };
        let document = automerge::Automerge::load(&bytes)
            .map_err(|error| format!("this device's copy of the record does not open: {error}"))?;
        let known: Vec<_> = after
            .into_iter()
            .filter(|head| document.get_change_meta_by_hash(head).is_some())
            .collect();
        let changes: Vec<u8> = document
            .get_changes(&known)
            .iter()
            .flat_map(|change| change.raw_bytes().iter().copied())
            .collect();
        let start = usize::try_from(from)
            .ok()
            .filter(|start| *start <= changes.len())
            .ok_or("the page starts past the record's changes")?;
        let end = changes.len().min(start.saturating_add(RECORD_PAGE_BYTES));
        Ok(Reply::RecordChanges {
            changes: Bytes(changes[start..end].to_vec()),
            heads: document
                .get_heads()
                .iter()
                .map(ToString::to_string)
                .collect(),
            next: (end < changes.len()).then(|| u64::try_from(end).unwrap_or(u64::MAX)),
        })
    }

    /// Write `changes`, the app's own changes of its `record` in `workspace`, concatenated, as
    /// this device's write: read only through the confined parser, then sealed, signed and
    /// committed as every write this device makes is, under the action's `commit` when it wrote
    /// more than one record. The record's first changes found it.
    pub(in crate::workspaces) async fn opened_record_change(
        &self,
        workspace: &str,
        (app, record): (&str, &str),
        changes: &[u8],
        commit: Option<&collab_types::WorkspaceCommit>,
        founding_refusal: Option<&str>,
    ) -> Result<(), String> {
        let parsed = tilo_automerge_isolate::parse_changes(changes)
            .map_err(|error| format!("the record's changes do not read: {error}"))?;
        if parsed.is_empty() {
            return Ok(());
        }
        let founding = !self.holds_resource(workspace, app, record);
        // A device that keeps the workspace for its owner admits no new record of its own: the
        // owning device does.
        if founding
            && let Some(refusal) = founding_refusal
            && !self.is_member(workspace).await?
        {
            return Err(refusal.to_owned());
        }
        let bytes = if founding {
            let mut document = automerge::Automerge::new();
            document
                .apply_changes(parsed)
                .map_err(|error| format!("the record's changes do not make a record: {error}"))?;
            if !document.get_missing_deps(&[]).is_empty() {
                return Err(
                    "the record's changes build on changes this device holds none of".into(),
                );
            }
            document.save()
        } else {
            parsed
                .iter()
                .flat_map(|change| change.raw_bytes().iter().copied())
                .collect()
        };
        self.write_as_device(
            workspace,
            (app, record),
            None,
            &bytes,
            (founding, founding),
            None,
            commit,
        )
        .await
        .map(drop)
        .map_err(|refusal| refusal.to_string())
    }
}

#[cfg(all(test, feature = "agents"))]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a test failing is its assertion failing"
    )]

    use automerge::transaction::Transactable as _;
    use tilo_resource_store::wire::{RECORD_PAGE_BYTES, Reply};

    use crate::workspaces::OwnWorkspaceOffer;
    use crate::workspaces::fixtures::{governor, test_host};

    const NOTES: &str = "tilo.notes";

    /// `len` characters of text that compresses poorly.
    fn noise(len: usize, seed: u64) -> String {
        const LETTERS: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                char::from(LETTERS[usize::try_from(state >> 58).unwrap()])
            })
            .collect()
    }

    /// An app whose manifest does not declare a record's kind cannot found one in a workspace
    /// this device governs, and nothing of the record is kept; a declared kind founds it, and
    /// the record then takes changes either way.
    #[tokio::test(flavor = "current_thread")]
    async fn an_undeclared_kind_founds_no_record() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let host = test_host(dir.path(), &key, &cert).await;
        let workspaces = host.workspaces();
        workspaces
            .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
            .unwrap();
        let workspace = workspaces.create("Kitchen").await.unwrap().workspace;
        let mut note = automerge::AutoCommit::new();
        note.put(automerge::ROOT, "title", "Plan").unwrap();
        let change = note.save();

        let refusal = "tilo.notes does not declare the resource kind `tilo.recipe`";
        let refused = workspaces
            .opened_record_change(&workspace, (NOTES, "plan"), &change, None, Some(refusal))
            .await;
        assert_eq!(refused, Err(refusal.to_owned()));
        let Reply::RecordChanges { heads, .. } = workspaces
            .opened_record_changes(&workspace, (NOTES, "plan"), &[], 0)
            .await
            .unwrap()
        else {
            panic!("a read answers the record's changes");
        };
        assert!(heads.is_empty(), "the refused record is not kept");

        workspaces
            .opened_record_change(&workspace, (NOTES, "plan"), &change, None, None)
            .await
            .expect("a declared kind founds the record");
        let heads = note.get_heads();
        note.put(automerge::ROOT, "title", "Plan B").unwrap();
        // Committed as an app's editor commits its own edits: a native change, its signer's.
        note.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Native.change_message()),
        );
        workspaces
            .opened_record_change(
                &workspace,
                (NOTES, "plan"),
                &note.save_after(&heads),
                None,
                Some(refusal),
            )
            .await
            .expect("the rule is the founding's: a kept record takes changes");
    }

    /// A record larger than one page is written in two changes, the first founding it and the
    /// second larger than one frame, and read back page by page into a copy that holds all of it.
    #[tokio::test(flavor = "current_thread")]
    async fn a_record_larger_than_a_frame_is_written_and_read_in_pages() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let host = test_host(dir.path(), &key, &cert).await;
        let workspaces = host.workspaces();
        workspaces
            .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
            .unwrap();
        let workspace = workspaces.create("Kitchen").await.unwrap().workspace;

        let mut note = automerge::AutoCommit::new();
        let mut changes = Vec::new();
        for (field, len, seed) in [("draft", 1_400_000, 1), ("revision", 4_500_000, 2)] {
            let heads = note.get_heads();
            note.put(automerge::ROOT, field, noise(len, seed)).unwrap();
            note.commit_with(
                automerge::transaction::CommitOptions::default()
                    .with_message(tilo_actions::FrameworkChange::Native.change_message()),
            );
            changes.push(note.save_after(&heads));
        }
        for change in &changes {
            workspaces
                .opened_record_change(&workspace, (NOTES, "plan"), change, None, None)
                .await
                .expect("the device writes the app's change as its own");
        }

        let page = |after: Vec<String>, from: u64| {
            let workspace = workspace.clone();
            async move {
                match workspaces
                    .opened_record_changes(&workspace, (NOTES, "plan"), &after, from)
                    .await
                {
                    Ok(Reply::RecordChanges {
                        changes,
                        heads,
                        next,
                    }) => (changes.0, heads, next),
                    other => panic!("a page of the record's changes, not {other:?}"),
                }
            }
        };
        assert!(
            changes[1].len() > collab_types::MAX_DOCUMENT_MUTATION_BYTES,
            "the second change is larger than one frame"
        );
        let (mut joined, heads, mut next) = page(Vec::new(), 0).await;
        let mut pages = 1;
        while let Some(from) = next {
            assert_eq!(
                u64::try_from(joined.len()).unwrap(),
                from,
                "each page is full but the last, and the next starts where it ends"
            );
            let (more, more_heads, after) = page(Vec::new(), from).await;
            assert!(!more.is_empty());
            assert_eq!(
                more_heads, heads,
                "every page names the whole answer's heads"
            );
            joined.extend(more);
            next = after;
            pages += 1;
        }
        assert_eq!(pages, joined.len().div_ceil(RECORD_PAGE_BYTES));
        assert!(pages >= 3, "{pages} pages");

        let mut copy = automerge::Automerge::new();
        copy.load_incremental(&joined)
            .expect("the pages join into the record's changes");
        let source: Vec<String> = note.get_heads().iter().map(ToString::to_string).collect();
        assert_eq!(heads, source, "the record holds what the app wrote");
        assert_eq!(
            copy.get_heads()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            source,
            "the copy holds all of it"
        );

        assert_eq!(
            page(heads.clone(), 0).await,
            (Vec::new(), heads.clone(), None),
            "nothing past what the asker holds"
        );
        assert!(
            workspaces
                .opened_record_changes(&workspace, (NOTES, "plan"), &[], u64::MAX)
                .await
                .is_err(),
            "a page past the end is refused"
        );
    }
}

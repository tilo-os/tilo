//! A store's reads and writes of its records as the workspace's record commands carry them
//! (`collab_types::store::wire::Command::RecordChanges`, `RecordChange` and `Records`): what
//! every [`DocumentPort`] that reaches the workspace by sending it commands reads and writes
//! with, whichever connection carries them. A record travels in pages and runs of at most
//! [`RECORD_PAGE_BYTES`], as a window's own record does on its route.

use automerge::ChangeHash;
use collab_types::store::wire::{
    Bytes, Command, RECORD_PAGE_BYTES, RecordPage, RecordPages, Reply,
};

use super::{DocumentAsk, DocumentPort, DocumentRead, Written};

/// Sends one command to the workspace and answers its reply.
pub type Ask<'a> = &'a dyn Fn(Command) -> Result<Reply, String>;

/// What the workspace holds of `wanted`, read page by page: on its way while the workspace says
/// so, nothing when it holds nothing, the changes past the reader's heads when it names them,
/// else the record whole. A page naming other heads than the first belongs to a newer answer,
/// which is read again from its start.
///
/// # Errors
/// The workspace could not be asked, refused the read, or answered something else.
pub fn read(ask: Ask<'_>, wanted: &DocumentAsk) -> Result<DocumentRead, String> {
    let since = wanted.since.as_deref().filter(|since| !since.is_empty());
    let after: Vec<String> = since
        .unwrap_or_default()
        .iter()
        .map(|head| ChangeHash(*head).to_string())
        .collect();
    let (mut pages, mut from) = (RecordPages::default(), 0);
    let (changes, heads) = loop {
        let reply = ask(Command::RecordChanges {
            record: Some(wanted.id.clone()),
            after: after.clone(),
            from,
        })?;
        match pages.join(reply) {
            Ok(RecordPage::From(next)) => from = next,
            Ok(RecordPage::Pending) => return Ok(DocumentRead::Pending),
            Ok(RecordPage::Done { changes, heads }) => break (changes, heads),
            Err(reply) => return Err(format!("the workspace answered a read with {reply:?}")),
        }
    };
    let heads = heads
        .iter()
        .map(|head| {
            head.parse::<ChangeHash>()
                .map_err(|_| format!("the workspace named {head}, which is no change"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if heads.is_empty() {
        return Ok(DocumentRead::Absent);
    }
    let revision = collab_types::document::revision(&heads);
    let heads: Vec<[u8; 32]> = heads.iter().map(|head| head.0).collect();
    Ok(match since {
        Some(since) if changes.is_empty() && since == heads.as_slice() => {
            DocumentRead::Unchanged(revision)
        }
        Some(_) => DocumentRead::Changes {
            revision,
            heads,
            bytes: changes,
        },
        None => DocumentRead::Whole {
            revision,
            heads,
            bytes: changes,
            hint: None,
        },
    })
}

/// Hand the workspace `changes` of the record `id`, in runs, each but the last with `more`: the
/// workspace takes them as one write once the last arrives. A record's first write is every
/// change it holds, which founds it.
///
/// # Errors
/// The workspace could not be asked, refused the write, or answered something else.
pub fn write(ask: Ask<'_>, id: &str, changes: &[u8]) -> Result<(), String> {
    let runs: Vec<&[u8]> = changes.chunks(RECORD_PAGE_BYTES).collect();
    for (index, run) in runs.iter().enumerate() {
        match ask(Command::RecordChange {
            record: Some(id.to_owned()),
            changes: Bytes(run.to_vec()),
            more: index + 1 < runs.len(),
            commit: None,
        })? {
            Reply::Written => {}
            other => return Err(format!("the workspace answered a write with {other:?}")),
        }
    }
    Ok(())
}

/// Every record of the asking app the workspace holds, each [`DocumentRead::Held`].
///
/// # Errors
/// The workspace could not be asked, refused, or answered something else.
pub fn list(ask: Ask<'_>) -> Result<Vec<(String, DocumentRead)>, String> {
    match ask(Command::Records)? {
        Reply::Ids(ids) => Ok(ids.into_iter().map(|id| (id, DocumentRead::Held)).collect()),
        other => Err(format!("the workspace answered a listing with {other:?}")),
    }
}

/// `app`'s `reads` in `workspace` (its listing when `reads` names none), each record read in
/// turn through `ask`.
///
/// # Errors
/// As [`read`] and [`list`].
pub fn answer(
    ask: Ask<'_>,
    reads: Option<Vec<DocumentAsk>>,
) -> Result<Vec<(String, DocumentRead)>, String> {
    let Some(reads) = reads else {
        return list(ask);
    };
    reads
        .into_iter()
        .map(|wanted| Ok((wanted.id.clone(), read(ask, &wanted)?)))
        .collect()
}

/// A port over a connection that answers each command as it is sent: `ask` takes the app and
/// workspace the command is of, and the command.
pub struct CommandDocuments<F>(pub F);

impl<F> DocumentPort for CommandDocuments<F>
where
    F: Fn(&str, &str, Command) -> Result<Reply, String>,
{
    fn read(
        &self,
        app: &str,
        workspace: &str,
        reads: Option<Vec<DocumentAsk>>,
    ) -> Result<Vec<(String, DocumentRead)>, String> {
        answer(&|command| (self.0)(app, workspace, command), reads)
    }

    fn write(
        &self,
        app: &str,
        workspace: &str,
        id: &str,
        _kind: &str,
        changes: Vec<u8>,
    ) -> Written {
        let mut answer = Some(write(
            &|command| (self.0)(app, workspace, command),
            id,
            &changes,
        ));
        Box::new(move |_| answer.take())
    }
}

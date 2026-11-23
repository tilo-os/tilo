//! What a person is shown of a message an agent of theirs wrote, before it
//! leaves: the question its send waits on.
//!
//! What they allow has to be what is sent. So the question is made from the
//! message as it was read for sending ([`Leaving`]), and the send that
//! follows their answer is handed that very reading:
//!
//! - every address the envelope will carry stands in a row of its own, as
//!   the address itself, and the question says how many there are. A name
//!   beside an address is not shown in its place;
//! - an address a person could misread (letters outside plain ASCII, a
//!   quoted first part, one too long for a row) is not put to them at all,
//!   and neither is a message to more addresses than a question shows: the
//!   send is refused, with their number, until all of them can be shown;
//! - what is shown in part says so in a row of its own, which no message
//!   can write: how much of the subject and of the message follows, and
//!   what a file or an invitation is without showing it;
//! - a second version of the message that a reader's mail program would
//!   show in place of these words is refused, since nothing here shows it.
//!
//! The rows are the whole of what is asked ([`Shown::rows`]): whatever puts
//! the question to the person, the confirmation their devices answer today
//! or a waiting question in a conversation, carries the same rows and lets
//! the same reading leave.

use tilo_actions::remote::Caller;
use tilo_actions::remote::person_accounts::SendFile;
use tilo_mail_session::Leaving;

use tilo_server_protocol::certify::PendingConfirmation;

/// How long a message waits for its person's answer before the question is
/// a refusal.
const WAIT_NS: u64 = 2 * 60 * 1_000_000_000;
/// The most addresses one question names.
pub(super) const MOST_ADDRESSES: usize = 20;
/// The longest address, and the longest sender's name, a row shows whole.
pub(super) const ADDRESS_CHARS: usize = 100;
/// How much of a subject, of a message and of a file's name a question
/// shows before it says that more follows.
const SUBJECT_CHARS: usize = 200;
const MESSAGE_CHARS: usize = 600;
const FILE_NAME_CHARS: usize = 80;
/// The most a question's rows hold together: every waiting question of a
/// person travels to their device in one frame of sixteen kilobytes.
pub(super) const QUESTION_BYTES: usize = 6 * 1024;

/// What a person is shown of one message before it leaves.
pub(super) struct Shown {
    account: String,
    rows: Vec<(String, String)>,
}

impl Shown {
    /// What `leaving` shows its person, with the `files` it carries and the
    /// calendar `invitation` it brings.
    ///
    /// # Errors
    /// Why the message cannot be shown so that what its person allows is
    /// what leaves; nobody is asked about such a message.
    pub(super) fn of(
        leaving: &Leaving,
        files: &[SendFile],
        invitation: Option<&str>,
    ) -> Result<Self, String> {
        let message = leaving.message();
        if message.html_body.is_some() {
            return Err(
                "A message an assistant sends leaves as the plain words its person is shown \
                 before it goes. This one carries a formatted version beside them, which a \
                 reader would see in their place: send it without one."
                    .into(),
            );
        }
        let addresses = leaving.recipients().count();
        if addresses > MOST_ADDRESSES {
            return Err(format!(
                "This message goes to {addresses} addresses, and its person is shown at most \
                 {MOST_ADDRESSES} before a message leaves. Send it to fewer at a time."
            ));
        }
        let mut rows = vec![("From".to_owned(), plain(leaving.sender())?)];
        if let Some(name) = message.from_name.as_deref().map(str::trim)
            && !name.is_empty()
        {
            if name.chars().count() > ADDRESS_CHARS {
                return Err(format!(
                    "The sender's name is longer than the {ADDRESS_CHARS} characters its \
                     person is shown before a message leaves."
                ));
            }
            rows.push(("Sender name".to_owned(), name.to_owned()));
        }
        rows.push((
            "Goes to".to_owned(),
            counted(addresses, "address", "addresses"),
        ));
        let lists = [
            ("To", leaving.to()),
            ("Copy to", leaving.cc()),
            ("Blind copy to", leaving.bcc()),
        ];
        for (label, list) in lists {
            for recipient in list {
                rows.push((label.to_owned(), plain(recipient.address())?));
            }
        }
        by_its_start(&mut rows, "Subject", message.subject.trim(), SUBJECT_CHARS);
        by_its_start(&mut rows, "Message", message.body.trim(), MESSAGE_CHARS);
        for file in files {
            rows.push(("File".to_owned(), named_file(file)));
        }
        if let Some(invitation) = invitation {
            rows.push((
                "Invitation".to_owned(),
                format!(
                    "A calendar invitation of {} is attached and not shown here",
                    counted(invitation.chars().count(), "character", "characters")
                ),
            ));
        }
        let bytes: usize = rows
            .iter()
            .map(|(label, value)| label.len().saturating_add(value.len()))
            .sum();
        if bytes > QUESTION_BYTES {
            return Err(
                "This message names more than one question can show its person before it \
                 leaves: shorter names for its files, or fewer of them, fit."
                    .into(),
            );
        }
        Ok(Self {
            account: message.account.clone(),
            rows,
        })
    }

    /// Everything the person is asked about, each row by its label and what
    /// it says, in the order they read it.
    pub(super) fn rows(&self) -> &[(String, String)] {
        &self.rows
    }

    /// The question as a confirmation the person's devices answer, asked by
    /// `agent` as `caller`.
    pub(super) fn question(&self, agent: &str, caller: &Caller) -> PendingConfirmation {
        PendingConfirmation {
            id: crate::workspaces::actions::agents::question_id(),
            agent_id: agent.to_owned(),
            agent_label: caller.agent_name().unwrap_or(agent).to_owned(),
            title: String::new(),
            action_label: "send this message".into(),
            resource_label: self.account.clone(),
            arguments: self.rows().to_vec(),
            risk: "sensitive".into(),
            kind: "outside".into(),
            undoable: false,
            workspace: String::new(),
            expires_at_ns: tilo_clock::unix_now_ns().saturating_add(WAIT_NS),
            page: None,
            ask: None,
            allow_always: false,
        }
    }
}

/// Whether a person reads `address` for what it is at a glance: plain ASCII,
/// one `@`, nothing quoted, and short enough to stand whole in a row.
pub(super) fn reads_plainly(address: &str) -> bool {
    const FIRST_PART: &[u8] = b".!#$%&'*+/=?^_`{|}~-";
    let readable = address.split_once('@').is_some_and(|(user, domain)| {
        let user_reads = user
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || FIRST_PART.contains(&byte));
        let domain_reads = domain
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-');
        !user.is_empty() && !domain.is_empty() && user_reads && domain_reads
    });
    readable && address.len() <= ADDRESS_CHARS
}

/// `address` as a row shows it, when a person reads it for what it is at a
/// glance ([`reads_plainly`]).
fn plain(address: &str) -> Result<String, String> {
    if reads_plainly(address) {
        return Ok(address.to_owned());
    }
    let start: String = address.chars().take(ADDRESS_CHARS).collect();
    Err(format!(
        "The address {start:?} cannot be shown to its person so that they read it for what it \
         is: it holds letters outside plain ASCII or a quoted part, or is longer than \
         {ADDRESS_CHARS} characters. A message to it is theirs to send."
    ))
}

/// Add the row `label` with `text`, whole when it is at most `most`
/// characters; else its start, and a row after it saying how much follows.
pub(super) fn by_its_start(rows: &mut Vec<(String, String)>, label: &str, text: &str, most: usize) {
    let all = text.chars().count();
    if all <= most {
        rows.push((label.to_owned(), text.to_owned()));
        return;
    }
    rows.push((label.to_owned(), text.chars().take(most).collect()));
    rows.push((
        format!("{label} continues"),
        format!(
            "{} are not shown here",
            counted(
                all.saturating_sub(most),
                "more character",
                "more characters"
            )
        ),
    ));
}

/// A file as its row names it: by its name, as far as a row shows one, and
/// its size.
fn named_file(file: &SendFile) -> String {
    let size = counted(
        usize::try_from(file.size).unwrap_or(usize::MAX),
        "byte",
        "bytes",
    );
    let all = file.filename.chars().count();
    if all <= FILE_NAME_CHARS {
        return format!("{} ({size})", file.filename);
    }
    let start: String = file.filename.chars().take(FILE_NAME_CHARS).collect();
    let more = counted(
        all.saturating_sub(FILE_NAME_CHARS),
        "more character",
        "more characters",
    );
    format!("{start} and {more} of its name ({size})")
}

/// `count` of something, in words.
pub(super) fn counted(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

//! A workspace's one store in this process's memory: what a host holds of documents it was handed
//! rather than read from a workspace service (a view attached to a record a page sent, a reader of
//! a record a host replayed, a headless run's profile), and what tests open, close and reopen
//! records in.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use super::{DocumentAsk, DocumentRead};
use automerge::AutoCommit;
use tilo_actions::surface::ReaderRule;
use tilo_automerge::ApplyIncoming as _;

use super::{DocumentPort, ReadyWake, Written};

/// The workspace's one store, as a port sees it: documents by app, workspace and id, merged as
/// writes arrive, refused once told to, and read as on their way while it is told it has not
/// answered for them.
#[derive(Default)]
pub struct MemoryWorkspace {
    documents: RefCell<BTreeMap<(String, String, String), AutoCommit>>,
    /// Who each document's last write named as its readers; absent for every member.
    readers: RefCell<BTreeMap<(String, String, String), String>>,
    /// Refuse every write from now on.
    pub refuse: Cell<bool>,
    /// How many writes came.
    pub writes: Cell<usize>,
    /// How many reads by id came.
    pub reads: Cell<usize>,
    /// The reader rule of each kind kept here ([`Self::declaring`]): the kinds whose records
    /// only some people read.
    rules: RefCell<BTreeMap<String, tilo_actions::surface::ReaderRule>>,
    /// The documents it has not answered for yet ([`Self::withhold`]).
    withheld: RefCell<BTreeSet<String>>,
    /// The apps whose documents come from the workspace's hosts ([`Self::await_hosts`]).
    from_hosts: RefCell<BTreeSet<String>>,
    /// The documents of those apps the hosts said they hold none of, by id
    /// ([`Self::hosts_hold_none`]).
    hold_none: RefCell<BTreeSet<String>>,
    /// What is woken once it answers for one ([`DocumentPort::when_answered`]).
    answer_wakes: RefCell<Vec<ReadyWake>>,
}

impl MemoryWorkspace {
    /// Keep `document` (a whole document, or changes to one) as `app`'s document `id` in
    /// `workspace`, merged with what is held of it: a document handed to the host rather than
    /// written through the port.
    ///
    /// # Errors
    /// `document` does not read.
    pub fn hold(
        &self,
        app: &str,
        workspace: &str,
        id: &str,
        document: &[u8],
    ) -> Result<(), String> {
        let key = (app.to_owned(), workspace.to_owned(), id.to_owned());
        let mut documents = self.documents.borrow_mut();
        let mut held = documents.remove(&key).unwrap_or_default();
        let loaded = held.load_incoming(document);
        // A document that never read is not held: a listing would name it, and a read answer it
        // empty.
        if !held.get_heads().is_empty() {
            documents.insert(key, held);
        }
        loaded.map(drop).map_err(|error| error.to_string())
    }

    /// Keep `surface`'s kind here by the readers its type names, as a device's store declares
    /// the records the system keeps: a write then records who the change says made it as the
    /// record's readers, so a reader of it is the person whose record it is.
    #[must_use]
    pub fn declaring(self, surface: tilo_actions::surface::Surface) -> Self {
        if let Some(rule) = surface.readers {
            self.rules.borrow_mut().insert(surface.kind, rule);
        }
        self
    }

    /// Who a write of `kind` names as its readers, from the kind's rule and `changes`: the person
    /// whose record it is, as [`tilo_actions::surface::ReaderRule::Author`] says. `None`
    /// for a kind every member reads.
    fn readers_by_rule(&self, kind: &str, changes: &[u8]) -> Option<String> {
        let rule = self.rules.borrow().get(kind).cloned()?;
        // Only the author's own record is derived here: a rule that opens with a field (until)
        // or names a role keeps its readers with the workspace's governance, which this store
        // does not hold.
        let ReaderRule::Author {
            until: None,
            here: false,
        } = rule
        else {
            return None;
        };
        let made = automerge::Automerge::parse_changes(changes).ok()?;
        let newest = made
            .iter()
            .filter_map(|change| change.message().map(str::to_owned))
            .filter_map(|message| tilo_actions::ActionContext::from_change_message(&message))
            .next_back()?;
        let acting = newest.invoker?.acting;
        let person = match acting.by.member_id() {
            Some(tilo_actions::MemberId::Person(person)) => person.as_str().to_owned(),
            _ => acting
                .acted_for
                .first()
                .and_then(|acting| acting.by.member_id())
                .and_then(|member| match member {
                    tilo_actions::MemberId::Person(person) => Some(person.as_str().to_owned()),
                    _ => None,
                })?,
        };
        Some(person.to_ascii_lowercase())
    }

    /// Leave reads of document `id` unanswered ([`DocumentRead::Pending`]) until [`Self::answer`]:
    /// a workspace whose answer for it is on its way.
    pub fn withhold(&self, id: &str) {
        self.withheld.borrow_mut().insert(id.to_owned());
    }

    /// Answer reads of document `id` from now on, with what it holds of it, and wake what waits
    /// for an answer it withheld.
    pub fn answer(&self, id: &str) {
        if self.withheld.borrow_mut().remove(id) {
            let wakes = std::mem::take(&mut *self.answer_wakes.borrow_mut());
            for wake in wakes {
                wake();
            }
        }
    }

    /// Take `app`'s documents from now on to come from the workspace's hosts, which send one
    /// only once it is asked for: a read of one not held here is answered as on its way
    /// ([`DocumentRead::Pending`]) until it is handed over ([`Self::hold`]) or the hosts said
    /// they hold none of it ([`Self::hosts_hold_none`]). What a host keeps of the documents a
    /// page or an attached view was sent, which never holds everything the workspace does.
    pub fn await_hosts(&self, app: &str) {
        self.from_hosts.borrow_mut().insert(app.to_owned());
    }

    /// The workspace's hosts said they hold none of document `id`: a read of it is answered as
    /// absent from now on, so whoever may start it starts it.
    pub fn hosts_hold_none(&self, id: &str) {
        self.hold_none.borrow_mut().insert(id.to_owned());
    }

    /// `app`'s document `id` in `workspace` whole, saved, when it is held: what another device
    /// that holds it sends.
    #[must_use]
    pub fn saved(&self, app: &str, workspace: &str, id: &str) -> Option<Vec<u8>> {
        let key = (app.to_owned(), workspace.to_owned(), id.to_owned());
        self.documents
            .borrow()
            .get(&key)
            .map(|doc| doc.clone().save())
    }

    /// Who the last write of `app`'s document `id` in `workspace` named as its readers: `None` for
    /// every member, and for a document never written.
    #[must_use]
    pub fn readers(&self, app: &str, workspace: &str, id: &str) -> Option<String> {
        self.readers
            .borrow()
            .get(&(app.to_owned(), workspace.to_owned(), id.to_owned()))
            .cloned()
    }
}

impl DocumentPort for MemoryWorkspace {
    fn read(
        &self,
        app: &str,
        workspace: &str,
        reads: Option<Vec<DocumentAsk>>,
    ) -> Result<Vec<(String, DocumentRead)>, String> {
        let documents = self.documents.borrow();
        // A whole read answers its bytes with the load hint for them, as the workspace store's
        // saves do: the reader skips rebuilding their change hashes. The document is kept by its
        // heads for the reader on this thread, which takes a copy of it rather than loading the
        // bytes again: a document handed to a host was loaded once, as it was handed over.
        let whole = |doc: &AutoCommit| {
            let bytes = doc.clone().save();
            tilo_automerge::shared::hold_at(bytes.len(), doc);
            let heads: Vec<[u8; 32]> = doc.committed_heads().iter().map(|head| head.0).collect();
            let hint = tilo_automerge::load_hint::encode(
                &bytes,
                doc.committed_document().change_hashes(),
                &doc.committed_heads(),
            );
            DocumentRead::Whole {
                revision: [0; 32],
                heads,
                bytes,
                hint,
            }
        };
        Ok(match reads {
            Some(reads) => {
                self.reads.set(self.reads.get() + 1);
                reads
                    .into_iter()
                    .map(|ask| {
                        let key = (app.to_owned(), workspace.to_owned(), ask.id.clone());
                        let read = if self.withheld.borrow().contains(&ask.id) {
                            DocumentRead::Pending
                        } else if let Some(doc) = documents.get(&key) {
                            whole(doc)
                        } else if self.from_hosts.borrow().contains(app)
                            && !self.hold_none.borrow().contains(&ask.id)
                        {
                            DocumentRead::Pending
                        } else {
                            DocumentRead::Absent
                        };
                        (ask.id, read)
                    })
                    .collect()
            }
            None => documents
                .iter()
                .filter(|((a, w, _), _)| a == app && w == workspace)
                .map(|((_, _, id), _)| (id.clone(), DocumentRead::Held))
                .collect(),
        })
    }

    fn when_answered(&self, wake: ReadyWake) -> bool {
        let withholds = !self.withheld.borrow().is_empty();
        if withholds {
            self.answer_wakes.borrow_mut().push(wake);
        }
        withholds
    }

    fn write(&self, app: &str, workspace: &str, id: &str, kind: &str, changes: Vec<u8>) -> Written {
        self.writes.set(self.writes.get() + 1);
        let answer = if self.refuse.get() {
            Err("the workspace refused it".to_owned())
        } else {
            let key = (app.to_owned(), workspace.to_owned(), id.to_owned());
            match self.readers_by_rule(kind, &changes) {
                Some(readers) => {
                    self.readers.borrow_mut().insert(key.clone(), readers);
                }
                None => {
                    self.readers.borrow_mut().remove(&key);
                }
            }
            // A founding write is the whole document, any other the changes after what is held:
            // both load the same way.
            self.hold(app, workspace, id, &changes)
        };
        let mut answer = Some(answer);
        Box::new(move |_| answer.take())
    }
}

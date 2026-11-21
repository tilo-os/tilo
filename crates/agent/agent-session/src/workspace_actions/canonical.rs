//! The registered reads a listing offers, kept between listings.
//!
//! A registered read runs over the host's retained source in a confined
//! worker mounting the record's component ([`reads::CanonicalRead`]): a new
//! process, the record's whole retained history replayed into it, its window
//! laid out. Every catalogue page lists each record's registered reads, so
//! preparing one per page made every listing cost that, and more as the
//! record grew. Most apps register no reader at all; an app is asked once
//! and then never mounted for its reads again. A record whose app does
//! register readers keeps its prepared read while the record's heads stay
//! where they were, checked against the caller's current policy each time.
//!
//! A kept read is a worker process too: what it answers, and the policy check
//! that decides whether it may, go through the same off-thread wait as any
//! other call ([`super::call_wait`]), so a large resource's read never holds the
//! thread that serves its workspace.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use tilo_actions::ActionDescriptor;
use tilo_actions::api::{ActionAddress, ApiError};
use tilo_actions::remote::Caller;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;

use super::{AppCatalogue, DocumentRoute, WorkspaceDocumentsLink, call_wait, reads};

/// Prepared reads kept at most: each is a worker process.
const KEPT: usize = 8;

/// A prepared read, and the heads of the record it was prepared at.
struct Kept {
    heads: Vec<[u8; 32]>,
    /// Shared with the thread a call on it goes on in ([`super::call_wait`]).
    read: Arc<Mutex<reads::CanonicalRead>>,
    used: u64,
}

/// What a caller asks a kept prepared read for.
pub(super) struct KeptAsk<'a> {
    pub(super) route: &'a DocumentRoute,
    /// The heads the service holds of its document.
    pub(super) heads: Option<&'a [[u8; 32]]>,
    pub(super) address: &'a ActionAddress,
    pub(super) arguments: &'a serde_json::Map<String, serde_json::Value>,
    /// The revision its caller named, which the read must stand at.
    pub(super) revision: Option<&'a str>,
}

/// What a kept read came to.
pub(super) enum KeptRead {
    Answered(tilo_actions::ActionOutput),
    /// It does not offer that read now: one is prepared.
    NotOffered,
    /// The resource moved past the revision its caller named.
    Stale,
    /// Its worker is still answering: the request waits off this thread for it.
    Held,
}

/// What a kept read answered when asked off the thread.
enum Looked {
    Answered(tilo_actions::ActionOutput),
    NotOffered,
    Stale,
}

#[derive(Default)]
pub(super) struct CanonicalReads {
    /// Apps whose component registers no reader.
    none: HashSet<String>,
    kept: HashMap<DocumentRoute, Kept>,
    clock: u64,
}

impl CanonicalReads {
    /// Whether `app_id`'s component is known to register no reader.
    pub(super) fn registers_none(&self, app_id: &str) -> bool {
        self.none.contains(app_id)
    }

    /// The registered reads of the record at `route` for `caller`, its
    /// record at `heads` (the heads the host holds of it, when it holds it):
    /// none for an app that registers no reader, the kept read's while the
    /// record has not moved, and otherwise a read prepared now.
    ///
    /// # Errors
    /// Why the read could not be prepared, or is not the caller's.
    pub(super) fn operations(
        &mut self,
        catalogue: &AppCatalogue,
        link: &Arc<dyn WorkspaceDocumentsLink>,
        (workspace, caller): (WorkspaceId, &Caller),
        route: &DocumentRoute,
        heads: Option<&[[u8; 32]]>,
        hand: &mut call_wait::Handing<'_>,
    ) -> Result<Vec<ActionDescriptor>, String> {
        if self.none.contains(&route.app_id) {
            return Ok(Vec::new());
        }
        self.clock = self.clock.wrapping_add(1);
        if let Some(heads) = heads
            && let Some(kept) = self.kept.get_mut(route)
            && kept.heads == heads
        {
            kept.used = self.clock;
            // The kept read answers from its warm worker, and checks the caller's policy there:
            // off this thread once the call outlasts one.
            let read = Arc::clone(&kept.read);
            let (link, caller, asked) = (Arc::clone(link), caller.clone(), route.clone());
            let asking = hand.hand(workspace, route, move || {
                let mut read = read.lock_recover();
                read.recheck(link.as_ref(), workspace, &caller, &asked)?;
                Ok::<_, String>(read.operations())
            });
            let Some(asking) = asking else {
                return Err("the app's worker is still answering".to_owned());
            };
            if let Ok(operations) = asking {
                return Ok(operations);
            }
        }
        self.kept.remove(route);
        // A read the service has not prepared yet is mounted in a worker, the document's whole
        // retained history replayed into it: it goes on off this thread once the call outlasts
        // one, and this request takes its answer when it is applied again.
        let prepared = {
            let (catalogue, link, caller, asked) = (
                catalogue.clone(),
                Arc::clone(link),
                caller.clone(),
                route.clone(),
            );
            hand.hand(workspace, route, move || {
                let mut read = reads::CanonicalRead::prepare(
                    &catalogue,
                    link.as_ref(),
                    workspace,
                    &caller,
                    &asked,
                )?;
                if !read.registers()? {
                    return Ok::<_, String>(None);
                }
                let operations = read.operations();
                Ok(Some((read, operations)))
            })
        };
        let Some(prepared) = prepared else {
            return Err("the app's worker is still answering".to_owned());
        };
        let Some((read, operations)) = prepared? else {
            self.none.insert(route.app_id.clone());
            return Ok(Vec::new());
        };
        if let Some(heads) = heads {
            while self.kept.len() >= KEPT {
                let Some(oldest) = self
                    .kept
                    .iter()
                    .min_by_key(|(_, kept)| kept.used)
                    .map(|(route, _)| route.clone())
                else {
                    break;
                };
                self.kept.remove(&oldest);
            }
            self.kept.insert(
                route.clone(),
                Kept {
                    heads: heads.to_vec(),
                    read: Arc::new(Mutex::new(read)),
                    used: self.clock,
                },
            );
        }
        Ok(operations)
    }

    /// Let go of what is kept of `workspace`'s records.
    pub(super) fn forget(&mut self, of: impl Fn(&DocumentRoute) -> bool) {
        self.kept.retain(|route, _| !of(route));
    }

    /// The kept read of the document at `route` for `caller` that offers `ask`, done for them now:
    /// its answer, or that none is kept or offers it ([`KeptRead`]). Kept between calls while the
    /// document stands where it was: the read a caller invoking it reuses rather than preparing
    /// its own.
    ///
    /// # Errors
    /// why the call was refused, for a read its component would not do.
    pub(super) fn invoke_kept(
        &mut self,
        link: &Arc<dyn WorkspaceDocumentsLink>,
        workspace: WorkspaceId,
        caller: &Caller,
        ask: &KeptAsk<'_>,
        hand: &mut call_wait::Handing<'_>,
    ) -> Result<KeptRead, ApiError> {
        if self.none.contains(&ask.route.app_id) {
            return Ok(KeptRead::NotOffered);
        }
        let Some(heads) = ask.heads else {
            return Ok(KeptRead::NotOffered);
        };
        self.clock = self.clock.wrapping_add(1);
        let Some(kept) = self
            .kept
            .get_mut(ask.route)
            .filter(|kept| kept.heads == heads)
        else {
            return Ok(KeptRead::NotOffered);
        };
        kept.used = self.clock;
        // The prepared read checks the caller's policy, offers the read and does it, all in its
        // own worker: off this thread once the call outlasts one.
        let read = Arc::clone(&kept.read);
        let (link, caller, asked) = (Arc::clone(link), caller.clone(), ask.route.clone());
        let (address, arguments) = (ask.address.clone(), ask.arguments.clone());
        let revision = ask.revision.map(str::to_owned);
        let looked = hand.hand(workspace, ask.route, move || {
            let mut read = read.lock_recover();
            if read
                .recheck(link.as_ref(), workspace, &caller, &asked)
                .is_err()
            {
                return Ok(Looked::NotOffered);
            }
            if revision
                .as_deref()
                .is_some_and(|revision| revision != read.revision())
            {
                return Ok(Looked::Stale);
            }
            if !read
                .operations()
                .iter()
                .any(|entry| entry.address == address)
            {
                return Ok(Looked::NotOffered);
            }
            read.invoke(
                link.as_ref(),
                workspace,
                &caller,
                &asked,
                &address,
                &arguments,
            )
            .map(Looked::Answered)
        });
        let Some(looked) = looked else {
            return Ok(KeptRead::Held);
        };
        Ok(match looked? {
            Looked::Answered(output) => KeptRead::Answered(output),
            Looked::NotOffered => KeptRead::NotOffered,
            Looked::Stale => KeptRead::Stale,
        })
    }
}

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

use std::collections::{HashMap, HashSet};

use tilo_actions::ActionDescriptor;
use tilo_actions::remote::Caller;
use tilo_relay_protocol::WorkspaceId;

use super::{AppCatalogue, DocumentRoute, WorkspaceRecordsLink, reads};

/// Prepared reads kept at most: each is a worker process.
const KEPT: usize = 8;

/// A prepared read, and the heads of the record it was prepared at.
struct Kept {
    heads: Vec<[u8; 32]>,
    read: reads::CanonicalRead,
    used: u64,
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
        link: &dyn WorkspaceRecordsLink,
        (workspace, caller): (WorkspaceId, &Caller),
        route: &DocumentRoute,
        heads: Option<&[[u8; 32]]>,
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
            if kept.read.recheck(link, workspace, caller, route).is_ok() {
                return Ok(kept.read.operations());
            }
        }
        self.kept.remove(route);
        let mut read = reads::CanonicalRead::prepare(catalogue, link, workspace, caller, route)?;
        if !read.registers()? {
            self.none.insert(route.app_id.clone());
            return Ok(Vec::new());
        }
        let operations = read.operations();
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
                    read,
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
}

//! The shell's own resources, kept as the person's: where they live, and how a change of them is
//! made.
//!
//! The shell keeps the person's shell choices in their own workspace (`resources: .own`): the
//! style, the bar and the dock, the strips' and the frames' settings. The device's daemon keeps
//! that workspace and carries it to the person's other devices. A session the daemon started,
//! running the installed shell, holds no copy of its own: the store its parts draw from is what
//! the daemon holds of the shell's documents, taken in as it moves (`Shell::sync_resource`), and a
//! change the person makes is their call of the shell's action on the device's dispatch
//! (`Shell::own_calls`), the same call an agent's `shell#show-style` or a press on their desktop
//! page makes. So the person's shell is one set of documents, whoever changes it, wherever.
//!
//! A session no daemon started, or one showing this build's own copy of the shell (which the
//! daemon does not run), holds the shell's resources in memory for that session: the shell is a
//! default app, in every image's store and every workspace's apps, so a person's session runs the
//! installed one and their choices are kept where their other devices read them.

use std::rc::Rc;

use tilolang_tilo::resources::{LocalPort, ResourceStore};

/// The shell's resources in this session, and how what the person's workspace holds of them is
/// taken in.
pub(crate) struct ShellRecord {
    port: Rc<LocalPort>,
    /// The store the shell's parts draw from and write into, shared by every part.
    store: ResourceStore,
    /// Whether the store only shows what the person's workspace holds: a document taken in
    /// stands in place of the defaults this session drew before it came, never merged with them.
    shows_theirs: bool,
}

impl ShellRecord {
    /// The shell's resources, starting from the defaults the session's first part seeds by the
    /// package's migrations. `kept_elsewhere`: the person's workspace keeps them, and the store is
    /// only what this session draws of it.
    pub(crate) fn open(kept_elsewhere: bool) -> Self {
        let port = Rc::new(LocalPort::in_memory());
        let store = ResourceStore::new(Rc::clone(&port) as Rc<dyn tilolang_tilo::resources::Port>);
        Self {
            port,
            store,
            shows_theirs: kept_elsewhere,
        }
    }

    /// The store the shell's parts draw from and write into.
    #[must_use]
    pub(crate) fn store(&self) -> ResourceStore {
        self.store.clone()
    }

    /// Take in `theirs`, what another of the person's devices holds of the document `doc`, so
    /// this session's shell draws it. Whether it changed.
    pub(crate) fn take_in(&self, doc: &str, theirs: &[u8]) -> bool {
        let taken = if self.shows_theirs {
            self.port.replace(doc, theirs)
        } else {
            self.port.merge(doc, theirs)
        };
        match taken {
            Ok(changed) => {
                if changed {
                    // What was read of it before is not what it holds now.
                    self.store.moved();
                }
                changed
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "the shell's resources did not take in what another device holds"
                );
                false
            }
        }
    }
}

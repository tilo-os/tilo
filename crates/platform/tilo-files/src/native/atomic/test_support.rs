//! Scoped production-checkpoint observation and failure injection for tests.

use crate::native::atomic::FileIdentity;
use std::cell::RefCell;
use std::fs::Metadata;
use std::io;
use std::rc::Rc;

/// A production publication checkpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Checkpoint {
    StageCreated,
    BeforeDataFlush,
    DataFlushed,
    BeforePublish,
    PublishSubmitted,
    /// Failure here models a lost rename acknowledgment after native execution.
    /// Both backends report `PublicationUnknown`; `BeforeCommit` instead models
    /// failure after known publication and retains commit-only retry authority.
    AfterPublish,
    BeforeCommit,
    Committed,
}

/// Owned observations of a retained staging file.
#[derive(Debug)]
pub struct StageObservation {
    pub metadata: Metadata,
    pub identity: FileIdentity,
}

/// An observed production checkpoint.
#[derive(Debug)]
pub struct CheckpointEvent {
    pub checkpoint: Checkpoint,
    pub stage: Option<StageObservation>,
}

type Observer = Rc<dyn Fn(&CheckpointEvent)>;

#[derive(Default)]
struct Hooks {
    failure: Option<Failure>,
    observer: Option<Observer>,
}

struct Failure {
    checkpoint: Checkpoint,
    remaining: Option<usize>,
}

thread_local! {
    static HOOKS: RefCell<Hooks> = RefCell::new(Hooks::default());
}

struct RestoreFailure(Option<Failure>);

impl Drop for RestoreFailure {
    fn drop(&mut self) {
        HOOKS.with(|hooks| hooks.borrow_mut().failure = self.0.take());
    }
}

struct RestoreObserver(Option<Observer>);

impl Drop for RestoreObserver {
    fn drop(&mut self) {
        HOOKS.with(|hooks| hooks.borrow_mut().observer = self.0.take());
    }
}

/// Fail every matching checkpoint during `operation` on this thread.
///
/// Panics for observation-only checkpoints because requesting such a failure
/// is a test-programming error.
pub fn with_failure<T>(checkpoint: Checkpoint, operation: impl FnOnce() -> T) -> T {
    with_scoped_failure(
        Failure {
            checkpoint,
            remaining: None,
        },
        operation,
    )
}

/// Fail only the selected positive occurrence of a checkpoint on this thread.
///
/// Other matching occurrences proceed normally. Nested scopes restore the
/// previous failure and its remaining occurrence count, including on unwind.
///
/// # Panics
///
/// Panics when `occurrence` is zero or `checkpoint` is observation-only.
pub fn with_failure_on_nth<T>(
    checkpoint: Checkpoint,
    occurrence: usize,
    operation: impl FnOnce() -> T,
) -> T {
    assert!(occurrence > 0, "occurrence must be positive");
    with_scoped_failure(
        Failure {
            checkpoint,
            remaining: Some(occurrence),
        },
        operation,
    )
}

fn with_scoped_failure<T>(failure: Failure, operation: impl FnOnce() -> T) -> T {
    let checkpoint = failure.checkpoint;
    assert!(
        matches!(
            checkpoint,
            Checkpoint::StageCreated
                | Checkpoint::BeforeDataFlush
                | Checkpoint::BeforePublish
                | Checkpoint::PublishSubmitted
                | Checkpoint::AfterPublish
                | Checkpoint::BeforeCommit
        ),
        "{checkpoint:?} is observation-only"
    );
    let previous = HOOKS.with(|hooks| hooks.borrow_mut().failure.replace(failure));
    let restore = RestoreFailure(previous);
    let output = operation();
    drop(restore);
    output
}

/// Observe production checkpoints for `operation` on this thread.
pub fn with_observer<T>(
    observer: impl Fn(&CheckpointEvent) + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    let previous = HOOKS.with(|hooks| hooks.borrow_mut().observer.replace(Rc::new(observer)));
    let restore = RestoreObserver(previous);
    let output = operation();
    drop(restore);
    output
}

pub(crate) fn checkpoint(event: &CheckpointEvent) -> io::Result<()> {
    let observer = HOOKS.with(|hooks| hooks.borrow().observer.clone());
    if let Some(observer) = observer {
        observer(event);
    }
    let fail = HOOKS.with(|hooks| {
        let mut hooks = hooks.borrow_mut();
        let Some(failure) = hooks.failure.as_mut() else {
            return false;
        };
        if failure.checkpoint != event.checkpoint {
            return false;
        }
        match failure.remaining.as_mut() {
            None => true,
            Some(0) => false,
            Some(remaining) => {
                *remaining = remaining.saturating_sub(1);
                *remaining == 0
            }
        }
    });
    if fail {
        return Err(io::Error::other(format!(
            "injected publication failure at {:?}",
            event.checkpoint
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nth_failure_selects_one_real_publication() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("record");
        with_failure_on_nth(Checkpoint::BeforeCommit, 2, || {
            crate::native::atomic::replace(
                &path,
                b"first",
                crate::native::atomic::Privacy::OwnerOnly,
            )
            .unwrap();
            let error = crate::native::atomic::replace(
                &path,
                b"second",
                crate::native::atomic::Privacy::OwnerOnly,
            )
            .unwrap_err();
            assert_eq!(
                error.state(),
                crate::native::atomic::PublicationState::PublishedDurabilityUnknown
            );
            assert_eq!(std::fs::read(&path).unwrap(), b"second");
            crate::native::atomic::replace(
                &path,
                b"third",
                crate::native::atomic::Privacy::OwnerOnly,
            )
            .unwrap();
        });
    }

    #[test]
    fn nth_failure_restores_outer_count_after_return_and_unwind() {
        let event = || CheckpointEvent {
            checkpoint: Checkpoint::BeforePublish,
            stage: None,
        };
        with_failure_on_nth(Checkpoint::BeforePublish, 2, || {
            checkpoint(&event()).unwrap();
            with_failure_on_nth(Checkpoint::BeforePublish, 1, || {
                assert!(checkpoint(&event()).is_err());
            });
            let panic = std::panic::catch_unwind(|| {
                with_failure_on_nth(Checkpoint::BeforePublish, 3, || {
                    checkpoint(&event()).unwrap();
                    panic!("restore outer occurrence count");
                });
            });
            assert!(panic.is_err());
            assert!(checkpoint(&event()).is_err());
            checkpoint(&event()).unwrap();
        });
        checkpoint(&event()).unwrap();
    }
}

/// A Unix parent-provisioning checkpoint. `Synced` observes a successful native fsync.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProvisionCheckpoint {
    BeforeCreate,
    Created,
    BeforeSync,
    Synced,
}

/// Owned physical-path and optional held-identity observation; never a handle.
#[cfg(unix)]
#[derive(Debug)]
pub struct ProvisionEvent {
    pub checkpoint: ProvisionCheckpoint,
    pub path: std::path::PathBuf,
    pub identity: Option<FileIdentity>,
}

/// Synthetic submitted mkdir result. These exercise conservative classification,
/// not native EIO/crash recovery. `AfterSuccess` executes the real mkdir first.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
pub enum MkdirFault {
    BeforeNative(i32),
    AfterSuccess(i32),
}

#[cfg(unix)]
type ProvisionObserver = Rc<dyn Fn(&ProvisionEvent)>;
#[cfg(unix)]
#[derive(Default)]
struct ProvisionHooks {
    observer: Option<ProvisionObserver>,
    failure: Option<(ProvisionCheckpoint, std::path::PathBuf)>,
    mkdir: Option<(std::path::PathBuf, MkdirFault)>,
}
#[cfg(unix)]
thread_local! {
    static PROVISION_HOOKS: RefCell<ProvisionHooks> = RefCell::new(ProvisionHooks::default());
}
#[cfg(unix)]
struct RestoreProvision(Option<ProvisionHooks>);
#[cfg(unix)]
impl Drop for RestoreProvision {
    fn drop(&mut self) {
        if let Some(previous) = self.0.take() {
            PROVISION_HOOKS.with(|hooks| *hooks.borrow_mut() = previous);
        }
    }
}
#[cfg(unix)]
fn provision_scope<T>(
    change: impl FnOnce(&mut ProvisionHooks),
    operation: impl FnOnce() -> T,
) -> T {
    let previous = PROVISION_HOOKS.with(|hooks| {
        let mut hooks = hooks.borrow_mut();
        let previous = ProvisionHooks {
            observer: hooks.observer.clone(),
            failure: hooks.failure.clone(),
            mkdir: hooks.mkdir.clone(),
        };
        change(&mut hooks);
        previous
    });
    let _restore = RestoreProvision(Some(previous));
    operation()
}
/// Observe this thread's real provisioning operations, restoring on unwind.
#[cfg(unix)]
pub fn with_provision_observer<T>(
    observer: impl Fn(&ProvisionEvent) + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    provision_scope(|hooks| hooks.observer = Some(Rc::new(observer)), operation)
}
/// Fail the exact physical path/checkpoint, after its observer has run.
///
/// # Panics
///
/// Panics when `checkpoint` is the observation-only `Synced`.
#[cfg(unix)]
pub fn with_provision_failure<T>(
    checkpoint: ProvisionCheckpoint,
    path: &std::path::Path,
    operation: impl FnOnce() -> T,
) -> T {
    assert!(
        checkpoint != ProvisionCheckpoint::Synced,
        "Synced is observation-only"
    );
    provision_scope(
        |hooks| hooks.failure = Some((checkpoint, path.to_owned())),
        operation,
    )
}
/// Override the selected submitted native mkdir result for this thread.
#[cfg(unix)]
pub fn with_mkdir_result<T>(
    path: &std::path::Path,
    fault: MkdirFault,
    operation: impl FnOnce() -> T,
) -> T {
    provision_scope(
        |hooks| hooks.mkdir = Some((path.to_owned(), fault)),
        operation,
    )
}
#[cfg(unix)]
pub(crate) fn provision_checkpoint(event: &ProvisionEvent) -> io::Result<()> {
    let observer = PROVISION_HOOKS.with(|hooks| hooks.borrow().observer.clone());
    if let Some(observer) = observer {
        observer(event);
    }
    let fail = PROVISION_HOOKS.with(|hooks| {
        hooks
            .borrow()
            .failure
            .as_ref()
            .is_some_and(|(checkpoint, path)| {
                *checkpoint == event.checkpoint && *path == event.path
            })
    });
    if fail {
        Err(io::Error::other(format!(
            "injected provision failure at {:?}: {}",
            event.checkpoint,
            event.path.display()
        )))
    } else {
        Ok(())
    }
}
#[cfg(unix)]
pub(crate) fn mkdir_result(
    path: &std::path::Path,
    operation: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let fault = PROVISION_HOOKS.with(|hooks| {
        hooks
            .borrow()
            .mkdir
            .as_ref()
            .filter(|(selected, _)| selected == path)
            .map(|(_, fault)| *fault)
    });
    match fault {
        Some(MkdirFault::BeforeNative(code)) => Err(io::Error::from_raw_os_error(code)),
        Some(MkdirFault::AfterSuccess(code)) => {
            operation()?;
            Err(io::Error::from_raw_os_error(code))
        }
        None => operation(),
    }
}

/// Checkpoints immediately before real existing-file data/name barriers.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExistingCheckpoint {
    BeforeDataFlush,
    BeforeNamespaceFlush,
}
#[cfg(unix)]
type ExistingObserver = Rc<dyn Fn(ExistingCheckpoint)>;
#[cfg(unix)]
thread_local! {
    static EXISTING_HOOKS: RefCell<(Option<ExistingCheckpoint>, Option<ExistingObserver>)> = RefCell::new((None, None));
}
#[cfg(unix)]
struct RestoreExisting((Option<ExistingCheckpoint>, Option<ExistingObserver>));
#[cfg(unix)]
impl Drop for RestoreExisting {
    fn drop(&mut self) {
        EXISTING_HOOKS.with(|h| *h.borrow_mut() = self.0.clone());
    }
}
/// Scope an existing-object failure and/or observer on the actual commit thread.
#[cfg(unix)]
pub fn with_existing_hooks<T>(
    failure: Option<ExistingCheckpoint>,
    observer: impl Fn(ExistingCheckpoint) + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    let previous = EXISTING_HOOKS.with(|h| h.replace((failure, Some(Rc::new(observer)))));
    let _restore = RestoreExisting(previous);
    operation()
}
#[cfg(unix)]
pub(crate) fn existing_checkpoint(checkpoint: ExistingCheckpoint) -> io::Result<()> {
    let (failure, observer) = EXISTING_HOOKS.with(|h| h.borrow().clone());
    if let Some(observer) = observer {
        observer(checkpoint);
    }
    if failure == Some(checkpoint) {
        Err(io::Error::other(format!(
            "injected existing commit failure at {checkpoint:?}"
        )))
    } else {
        Ok(())
    }
}

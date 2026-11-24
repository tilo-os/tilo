//! Rust-owned network transports with operation-local repository configuration.

use std::cell::RefCell;
use std::sync::OnceLock;

thread_local! {
    static CONFIG: RefCell<Option<git2::Config>> = const { RefCell::new(None) };
}

/// Every application network operation must enter here before creating a
/// transport. Registration finishes before any such operation can proceed.
pub(crate) fn with_context<T>(
    config: git2::Config,
    operation: impl FnOnce() -> Result<T, git2::Error>,
) -> Result<T, git2::Error> {
    static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
    let registered = REGISTERED.get_or_init(|| {
        crate::http_transport::register()
            .and_then(|()| crate::ssh_transport::register())
            .map_err(|error| error.message().to_owned())
    });
    if let Err(message) = registered {
        return Err(git2::Error::from_str(message));
    }
    let previous = CONFIG.with(|slot| slot.replace(Some(config)));
    let _restore = Restore(previous);
    operation()
}

struct Restore(Option<git2::Config>);

impl Drop for Restore {
    fn drop(&mut self) {
        CONFIG.with(|slot| slot.replace(self.0.take()));
    }
}

/// Factories immediately convert this snapshot into owned transport settings;
/// a libgit2 Config itself must never be sent to a transport worker thread.
pub(super) fn current_config() -> Result<git2::Config, git2::Error> {
    CONFIG.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .ok_or_else(|| git2::Error::from_str("network operation has no repository context"))?
            .snapshot()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_context_is_thread_local_and_restores_after_return() {
        let workers: Vec<_> = ["alice", "bob"]
            .into_iter()
            .map(|name| {
                std::thread::spawn(move || {
                    let root = tempfile::tempdir().unwrap();
                    let repository = git2::Repository::init(root.path()).unwrap();
                    let mut config = repository.config().unwrap();
                    config.set_str("credential.username", name).unwrap();
                    assert!(current_config().is_err());
                    let actual = with_context(config, || {
                        current_config()?.get_string("credential.username")
                    })
                    .unwrap();
                    assert_eq!(actual, name);
                    assert!(current_config().is_err());
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    }
}

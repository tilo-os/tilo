//! Named data a host supplies on request, for builds that ship large
//! application data (fonts, archives) beside the program instead of inside
//! it. An application asks for a name each time it needs the bytes; a host
//! that can fetch drains the requests, fetches each, installs the bytes or a
//! failure, and relayouts its surfaces so the application asks again.
//! Builds that embed their data never ask, and a host that never drains
//! leaves every request pending.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tilo_lock::MutexRecover as _;

/// Where a requested name stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Requested; the host has not answered yet.
    Pending,
    Ready(Arc<[u8]>),
    /// The host could not supply it; stays failed for the process.
    Failed(String),
}

enum Entry {
    Requested,
    Fetching,
    Answered(Status),
}

static DATA: Mutex<BTreeMap<String, Entry>> = Mutex::new(BTreeMap::new());

fn with<R>(f: impl FnOnce(&mut BTreeMap<String, Entry>) -> R) -> R {
    f(&mut DATA.lock_recover())
}

/// The bytes for `name` if the host supplied them; otherwise records the
/// request (once) and reports it pending or failed.
pub fn request(name: &str) -> Status {
    with(|data| match data.get(name) {
        Some(Entry::Answered(status)) => status.clone(),
        Some(Entry::Requested | Entry::Fetching) => Status::Pending,
        None => {
            data.insert(name.to_owned(), Entry::Requested);
            Status::Pending
        }
    })
}

/// Names requested since the last call, for the host to fetch.
pub fn take_requests() -> Vec<String> {
    with(|data| {
        data.iter_mut()
            .filter(|(_, entry)| matches!(entry, Entry::Requested))
            .map(|(name, entry)| {
                *entry = Entry::Fetching;
                name.clone()
            })
            .collect()
    })
}

/// Supply `name`'s bytes.
pub fn install(name: &str, bytes: Vec<u8>) {
    with(|data| {
        data.insert(
            name.to_owned(),
            Entry::Answered(Status::Ready(bytes.into())),
        );
    });
}

/// Report that `name` cannot be supplied.
pub fn fail(name: &str, reason: &str) {
    with(|data| {
        data.insert(
            name.to_owned(),
            Entry::Answered(Status::Failed(reason.to_owned())),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_handed_to_the_host_once_and_answered() {
        let name = "test/answered.bin";
        assert_eq!(request(name), Status::Pending);
        assert_eq!(request(name), Status::Pending);
        assert_eq!(take_requests().iter().filter(|n| *n == name).count(), 1);
        assert!(
            !take_requests().iter().any(|n| n == name),
            "handed over once"
        );
        install(name, vec![1, 2, 3]);
        assert_eq!(request(name), Status::Ready(Arc::from(&[1u8, 2, 3][..])));
    }

    #[test]
    fn a_failure_stays_failed() {
        let name = "test/failed.bin";
        let _ = request(name);
        fail(name, "HTTP 404");
        assert_eq!(request(name), Status::Failed("HTTP 404".into()));
        assert!(!take_requests().iter().any(|n| n == name));
    }
}

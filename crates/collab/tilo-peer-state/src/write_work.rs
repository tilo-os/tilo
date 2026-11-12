//! The bytes committing a write reads, counted on each thread.
//!
//! A write must cost what it changed, however long its record's history.
//! Time is a poor witness of that (it varies with the machine and with
//! everything else running), so the steps that would read the history again
//! count the bytes they read instead: a test commits writes into a short
//! history and a long one and compares.

#[cfg(test)]
thread_local! {
    static READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Count `bytes` read again on this thread.
pub(crate) fn read(bytes: usize) {
    #[cfg(test)]
    READ.with(|read| read.set(read.get().saturating_add(bytes)));
    #[cfg(not(test))]
    let _ = bytes;
}

/// The bytes counted on this thread since the last call, which starts the
/// count over.
#[cfg(test)]
pub(crate) fn take() -> usize {
    READ.with(|read| read.replace(0))
}

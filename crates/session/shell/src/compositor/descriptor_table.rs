//! The compositor's file-descriptor table, sized once at startup.
//!
//! The kernel keeps a process's open files in one table and grows it when a
//! descriptor lands past its end: 64 slots, then 256, 512, 1024 and on. In a
//! process with more than one thread every growth waits for an RCU grace
//! period, and every other thread that opens a file meanwhile (a client
//! connecting, a buffer imported, an eventfd) sleeps until it is done. Under
//! load that is long: the desktop smoke under CPU and memory pressure found
//! the main loop asleep in `expand_files` for 282 ms, the pointer and the
//! keyboard waiting behind it.
//!
//! The compositor gains descriptors exactly when the person waits on it (an
//! app opening, a window appearing), so the table is grown once, before the
//! compositor starts a thread, to cover every descriptor its limit allows
//! (at most [`RESERVED_SLOTS`]). It never grows again, and it costs a few
//! pages.

/// The most slots reserved up front: 32 KiB of table. A limit above it is a
/// machine with room to spare, where a growth past it is rare.
pub const RESERVED_SLOTS: u64 = 4096;

/// How many slots [`reserve`] makes room for under a soft descriptor limit
/// of `soft_limit`: every descriptor the limit allows, up to
/// [`RESERVED_SLOTS`].
#[must_use]
pub fn slots_for(soft_limit: u64) -> u64 {
    soft_limit.min(RESERVED_SLOTS)
}

/// The process's soft limit on open descriptors.
fn soft_limit() -> Option<u64> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit` writes one `rlimit` through a pointer to a live,
    // correctly sized local.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
        return None;
    }
    Some(limit.rlim_cur)
}

/// Grow this process's descriptor table to [`slots_for`] its soft limit.
///
/// Duplicates a descriptor to the last slot and closes it again: the table
/// keeps its size, and no descriptor the process holds moves. Returns the
/// slots the table now has room for, or `None` when it was left as it was
/// (a failure here costs the head start, never the session).
pub fn reserve() -> Option<u64> {
    let slots = slots_for(soft_limit()?);
    let last = libc::c_int::try_from(slots.checked_sub(1)?).ok()?;
    // SAFETY: `eventfd` takes scalars and returns a descriptor this function
    // owns.
    let probe = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
    if probe < 0 {
        return None;
    }
    // SAFETY: `fcntl(F_DUPFD_CLOEXEC)` duplicates the descriptor owned above
    // to the lowest free slot at or past `last`; it never replaces an open
    // descriptor.
    let high = unsafe { libc::fcntl(probe, libc::F_DUPFD_CLOEXEC, last) };
    // SAFETY: both descriptors were opened here and are closed exactly once.
    unsafe {
        if high >= 0 {
            libc::close(high);
        }
        libc::close(probe);
    }
    (high >= 0).then_some(slots)
}

/// The slots this process's descriptor table has, as `/proc/<pid>/status`
/// reports them (`FDSize`).
#[must_use]
pub fn table_slots(status: &str) -> Option<u64> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("FDSize:"))?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reservation_covers_the_limit_up_to_its_cap() {
        assert_eq!(slots_for(1024), 1024);
        assert_eq!(slots_for(524_288), RESERVED_SLOTS);
        assert_eq!(slots_for(0), 0);
    }

    #[test]
    fn the_table_size_is_read_from_the_status_file() {
        let status = "Name:\tshell\nFDSize:\t1024\nGroups:\t10\n";
        assert_eq!(table_slots(status), Some(1024));
        assert_eq!(table_slots("Name:\tshell\n"), None);
    }

    #[test]
    fn reserving_grows_the_table_and_leaves_its_last_slot_free() {
        let slots = reserve().expect("reserve the descriptor table");
        assert_eq!(slots, slots_for(soft_limit().expect("read the limit")));
        let status = std::fs::read_to_string("/proc/self/status").expect("read status");
        let table = table_slots(&status).expect("FDSize in status");
        assert!(
            table >= slots,
            "the table has {table} slots after reserving {slots}"
        );
        let last = std::path::PathBuf::from(format!("/proc/self/fd/{}", slots.saturating_sub(1)));
        assert!(!last.exists(), "the slot reserved through stayed open");
    }
}

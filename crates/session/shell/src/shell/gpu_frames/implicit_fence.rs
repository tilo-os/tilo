//! Completion of the producer's implicit dma-buf write fence, never elapsed-time proof.
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::time::{Duration, Instant};

pub(super) struct Report {
    pub result: io::Result<()>,
    pub poll_calls: u32,
    pub events: i16,
}

pub(super) fn wait(fd: BorrowedFd<'_>, timeout: Duration) -> Report {
    let Some(deadline) = Instant::now().checked_add(timeout) else {
        return Report {
            result: Err(io::Error::from(io::ErrorKind::InvalidInput)),
            poll_calls: 0,
            events: 0,
        };
    };
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let mut poll_calls: u32 = 0;
    let result = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        let milliseconds = remaining.as_millis().saturating_add(1);
        let timeout = i32::try_from(milliseconds).unwrap_or(i32::MAX);
        poll_calls = poll_calls.saturating_add(1);
        // SAFETY: the descriptor is borrowed through this call; poll points to one valid pollfd.
        let ready = unsafe { libc::poll(&raw mut poll, 1, timeout) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break Err(error);
        }
        if ready == 0 {
            break Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        if poll.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            break Err(io::Error::other("producer fence descriptor is unavailable"));
        }
        if poll.revents & libc::POLLIN != 0 {
            break Ok(());
        }
        break Err(io::Error::other(
            "producer fence did not signal write completion",
        ));
    };
    Report {
        result,
        poll_calls,
        events: poll.revents,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;

    #[test]
    fn actual_readiness_is_required_before_the_import_can_continue() {
        let (read, mut write) = UnixStream::pair().unwrap();
        write.write_all(&[1]).unwrap();
        let report = wait(read.as_fd(), Duration::from_millis(20));
        report.result.unwrap();
        assert_eq!(report.poll_calls, 1);
        assert_ne!(report.events & libc::POLLIN, 0);
    }

    #[test]
    fn timeout_and_closed_producer_are_not_write_completion() {
        let (read, write) = UnixStream::pair().unwrap();
        let report = wait(read.as_fd(), Duration::from_millis(3));
        assert_eq!(report.result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(report.poll_calls, 1);
        drop(write);
        let report = wait(read.as_fd(), Duration::from_millis(20));
        assert!(report.result.is_err());
        assert_ne!(report.events & libc::POLLHUP, 0);
    }
}

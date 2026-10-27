//! TILO PATCH: wake on a resume or a clock step instead of polling for one.
//!
//! The actor used to read the clock every 15 s to notice that the machine
//! slept. On Linux that poll cannot see a sleep: `Instant` is
//! `CLOCK_MONOTONIC`, which stands still while the machine is suspended, so
//! the interval resumes where it left off and never measures a jump. It woke
//! the runtime four times a minute for nothing.
//!
//! A `timerfd` on `CLOCK_REALTIME` armed with `TFD_TIMER_CANCEL_ON_SET` is
//! cancelled by the kernel whenever the realtime clock is set and on every
//! resume (`timekeeping_resume` calls `timerfd_resume`), and does nothing
//! otherwise. A resume is told from a clock step by the time spent
//! suspended: the growth of `CLOCK_BOOTTIME` over `CLOCK_MONOTONIC`.

#[cfg(target_os = "linux")]
pub(super) use linux::ClockChange;
#[cfg(not(target_os = "linux"))]
pub(super) use unsupported::ClockChange;

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        io,
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
        time::Duration,
    };

    use tokio::io::{Interest, unix::AsyncFd};

    /// The shortest stretch of suspended time counted as a sleep. Reading two
    /// clocks one after the other leaves nanoseconds of jitter between them.
    const MIN_SUSPEND: Duration = Duration::from_secs(1);

    /// A resume or realtime clock step, as the kernel reports it.
    #[derive(Debug)]
    pub(in crate::netmon) struct ClockChange {
        fd: AsyncFd<OwnedFd>,
        /// `CLOCK_BOOTTIME - CLOCK_MONOTONIC` when last looked at.
        suspended: Duration,
    }

    impl ClockChange {
        pub(in crate::netmon) fn new() -> io::Result<Self> {
            // SAFETY: plain system call; the result is checked before use.
            let raw = unsafe {
                libc::timerfd_create(libc::CLOCK_REALTIME, libc::TFD_NONBLOCK | libc::TFD_CLOEXEC)
            };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `raw` is a descriptor this call just opened and owns.
            let fd = unsafe { OwnedFd::from_raw_fd(raw) };
            arm(&fd)?;
            // SAFETY: `fd` is the timerfd opened above, moved into the
            // AsyncFd. Nothing else holds it, so it stays open and refers to
            // the same timer until the AsyncFd deregisters it on drop.
            let fd = unsafe { AsyncFd::register_with_interest(fd, Interest::READABLE) }?;
            Ok(Self {
                fd,
                suspended: suspended_time()?,
            })
        }

        /// Waits for the next resume or realtime clock step. Returns whether
        /// the machine was suspended since the previous call.
        pub(in crate::netmon) async fn changed(&mut self) -> io::Result<bool> {
            loop {
                let mut ready = self.fd.readable().await?;
                match ready.try_io(|fd| read_expirations(fd.get_ref())) {
                    // Cancelled: the clock was set or the machine resumed.
                    Ok(Err(err)) if err.raw_os_error() == Some(libc::ECANCELED) => {
                        arm(self.fd.get_ref())?;
                        let suspended = suspended_time()?;
                        let slept = suspended.saturating_sub(self.suspended) >= MIN_SUSPEND;
                        self.suspended = suspended;
                        return Ok(slept);
                    }
                    Ok(Err(err)) => return Err(err),
                    // The timer itself expired (it is set to the end of
                    // time, so this does not happen): arm it again.
                    Ok(Ok(())) => arm(self.fd.get_ref())?,
                    Err(_would_block) => {}
                }
            }
        }
    }

    /// Arms the timer at the latest time there is, so it only ever reports a
    /// cancellation. Required again after every cancellation.
    fn arm(fd: &OwnedFd) -> io::Result<()> {
        let at = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: libc::timespec {
                tv_sec: libc::time_t::MAX,
                tv_nsec: 0,
            },
        };
        // SAFETY: `fd` is an open timerfd, `at` is valid for the call, and a
        // null old value is allowed.
        let res = unsafe {
            libc::timerfd_settime(
                fd.as_raw_fd(),
                libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET,
                &raw const at,
                std::ptr::null_mut(),
            )
        };
        if res < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn read_expirations(fd: &OwnedFd) -> io::Result<()> {
        let mut expirations = 0u64;
        // SAFETY: reads at most eight bytes into a live u64.
        let n = unsafe {
            libc::read(
                fd.as_raw_fd(),
                (&raw mut expirations).cast(),
                std::mem::size_of::<u64>(),
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Time spent suspended since boot.
    fn suspended_time() -> io::Result<Duration> {
        let monotonic = now(libc::CLOCK_MONOTONIC)?;
        let boottime = now(libc::CLOCK_BOOTTIME)?;
        Ok(boottime.saturating_sub(monotonic))
    }

    fn now(clock: libc::clockid_t) -> io::Result<Duration> {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a valid out parameter.
        if unsafe { libc::clock_gettime(clock, &raw mut ts) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let secs = u64::try_from(ts.tv_sec).unwrap_or_default();
        let nanos = u32::try_from(ts.tv_nsec).unwrap_or_default();
        Ok(Duration::new(secs, nanos))
    }
}

#[cfg(not(target_os = "linux"))]
mod unsupported {
    use std::io;

    /// No event source here: the actor keeps polling the clock.
    #[derive(Debug)]
    pub(in crate::netmon) struct ClockChange(());

    impl ClockChange {
        pub(in crate::netmon) fn new() -> io::Result<Self> {
            Err(io::ErrorKind::Unsupported.into())
        }

        pub(in crate::netmon) async fn changed(&mut self) -> io::Result<bool> {
            std::future::pending().await
        }
    }
}

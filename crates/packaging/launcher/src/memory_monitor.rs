//! The memory watch: samples a [`Source`] of cgroups, runs the policy
//! ([`crate::memory_pressure`]) and carries out what it decides through
//! [`Effects`]. The session runs exactly one watch over the apps it started
//! (`tilo_app_launch`'s memory module); it is the only memory watch there is.
//!
//! It sleeps while memory is calm. PSI triggers on `/proc/pressure/memory`
//! and on the apps' own `memory.pressure` (150 ms of stall within 2 s, the
//! finest window the kernel grants an unprivileged process) wake it; then it
//! samples once a second until pressure has been calm for [`CALM_SAMPLES`]
//! samples, and sleeps again. Where no trigger can be armed (an old kernel)
//! it samples every [`FALLBACK_INTERVAL`] instead.

use std::io;
use std::os::fd::{AsRawFd as _, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::memory_pressure::{Action, Policy, Psi, Unit, parse_psi};

/// The machine's memory pressure.
pub const SYSTEM_PRESSURE: &str = "/proc/pressure/memory";

/// The trigger armed on every pressure file: 150 ms of stall within a 2 s
/// window. Unprivileged triggers need a window of whole 2 s.
pub const TRIGGER: &str = "some 150000 2000000";

/// [`TRIGGER`] as written to a pressure file, NUL-terminated. The kernel's
/// `/proc/pressure/*` writer overwrites the last byte written with a NUL, so
/// an unterminated string loses its last digit: a 200 ms window, below the
/// 500 ms minimum, refused with `EINVAL`. A cgroup's `memory.pressure` reads
/// the string up to the NUL, so the same bytes arm both.
const TRIGGER_WRITTEN: &[u8] = b"some 150000 2000000\0";

/// Samples a second while pressure is up.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// Calm samples in a row after which the watch sleeps again.
pub const CALM_SAMPLES: u32 = 10;

/// How often to sample when no trigger could be armed.
pub const FALLBACK_INTERVAL: Duration = Duration::from_secs(2);

/// Where a sample's readings come from.
pub trait Source: Send {
    /// The machine's memory pressure; `None` when it cannot be read.
    fn system(&mut self) -> Option<Psi>;
    /// Every app cgroup watched.
    fn units(&mut self) -> Vec<Unit>;
    /// The pressure files whose stall should wake the watch besides the
    /// machine's own: those of the cgroups below what is watched.
    fn pressure_files(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// What carries the policy's decisions out.
pub trait Effects: Send {
    fn reclaim(&mut self, dir: &Path, bytes: u64);
    fn end(&mut self, unit: &Unit, growth: u64);
}

/// One sample: read, decide, act. Returns the pressure read, `None` when
/// there was none to read.
pub fn step(
    policy: &mut Policy,
    source: &mut dyn Source,
    effects: &mut dyn Effects,
) -> Option<Psi> {
    let system = source.system()?;
    let units = source.units();
    for action in policy.tick(system, &units) {
        match action {
            Action::Reclaim { dir, bytes } => effects.reclaim(&dir, bytes),
            Action::End { unit, growth } => effects.end(&unit, growth),
        }
    }
    Some(system)
}

/// The pressure of a PSI file.
pub fn read_psi(path: &Path) -> Option<Psi> {
    parse_psi(&std::fs::read_to_string(path).ok()?)
}

/// Arm a PSI trigger on `path`: the descriptor becomes `POLLPRI` when it
/// fires.
fn arm(path: &Path) -> io::Result<OwnedFd> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    file.write_all(TRIGGER_WRITTEN)?;
    Ok(OwnedFd::from(file))
}

/// A watch running on a thread of its own.
pub struct Watch {
    /// Written to end the watch at once: the thread waits on it with the
    /// pressure triggers.
    stop: OwnedFd,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watch {
    /// Watch pressure through `source`, carrying out what the policy decides
    /// with `effects`, on a thread named `name`.
    pub fn spawn(
        name: &str,
        source: Box<dyn Source>,
        effects: Box<dyn Effects>,
    ) -> io::Result<Self> {
        // SAFETY: eventfd takes the flags it documents and returns a new
        // descriptor this process owns alone.
        let stop = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if stop < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the kernel just returned this descriptor, and nothing else
        // owns it.
        let stop = unsafe {
            use std::os::fd::FromRawFd as _;
            std::os::fd::OwnedFd::from_raw_fd(stop)
        };
        let waiting = stop.try_clone()?;
        let thread = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || watch(source, effects, &waiting))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    /// End the watch now, and wait for its thread to notice.
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        let one = 1_u64.to_ne_bytes();
        // SAFETY: writing one event to the eventfd this value owns alone.
        unsafe { libc::write(self.stop.as_raw_fd(), one.as_ptr().cast(), one.len()) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.halt();
    }
}

fn watch(mut source: Box<dyn Source>, mut effects: Box<dyn Effects>, stop: &OwnedFd) {
    let mut watched = source.pressure_files();
    watched.insert(0, PathBuf::from(SYSTEM_PRESSURE));
    let mut triggers: Vec<OwnedFd> = watched
        .into_iter()
        .filter_map(|path| match arm(&path) {
            Ok(fd) => Some(fd),
            Err(error) => {
                eprintln!(
                    "tilo-app-memory: no pressure trigger on {}: {error}",
                    path.display()
                );
                None
            }
        })
        .collect();
    let mut policy = Policy::new();
    let mut calm_samples = CALM_SAMPLES;
    loop {
        let timeout = if calm_samples < CALM_SAMPLES {
            Some(SAMPLE_INTERVAL)
        } else if triggers.is_empty() {
            Some(FALLBACK_INTERVAL)
        } else {
            None
        };
        match wait(&mut triggers, stop.as_raw_fd(), timeout) {
            Wake::Stop => return,
            // Pressure the averages do not show yet: sample from now on.
            Wake::Trigger => calm_samples = 0,
            Wake::Timeout => {}
        }
        let Some(system) = step(&mut policy, &mut *source, &mut *effects) else {
            continue;
        };
        if Policy::calm(system) {
            calm_samples = calm_samples.saturating_add(1);
            if calm_samples == CALM_SAMPLES {
                // Asleep from here: start the next episode fresh.
                policy = Policy::new();
            }
        } else {
            calm_samples = 0;
        }
    }
}

/// What ended a wait.
enum Wake {
    /// A pressure trigger fired.
    Trigger,
    /// The timeout passed.
    Timeout,
    /// The watch was asked to end.
    Stop,
}

/// Sleep until a trigger fires, the watch is stopped, or `timeout` passes.
/// Drops a trigger that broke (its cgroup went away).
fn wait(triggers: &mut Vec<OwnedFd>, stop: RawFd, timeout: Option<Duration>) -> Wake {
    let mut fds: Vec<libc::pollfd> = triggers
        .iter()
        .map(|fd| libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLPRI,
            revents: 0,
        })
        .collect();
    fds.push(libc::pollfd {
        fd: stop,
        events: libc::POLLIN,
        revents: 0,
    });
    let millis = timeout.map_or(-1, |timeout| {
        i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX)
    });
    let count = libc::nfds_t::try_from(fds.len()).unwrap_or(0);
    // SAFETY: `fds` holds `count` valid pollfd entries.
    let ready = unsafe { libc::poll(fds.as_mut_ptr(), count, millis) };
    if ready <= 0 {
        return Wake::Timeout;
    }
    if fds
        .last()
        .is_some_and(|fd| fd.revents & (libc::POLLIN | libc::POLLERR | libc::POLLNVAL) != 0)
    {
        return Wake::Stop;
    }
    let mut fired = false;
    let mut broken = Vec::new();
    for (index, fd) in fds.iter().enumerate().take(triggers.len()) {
        if fd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            broken.push(index);
        } else if fd.revents & libc::POLLPRI != 0 {
            fired = true;
        }
    }
    for index in broken.into_iter().rev() {
        if index < triggers.len() {
            drop(triggers.remove(index));
        }
    }
    if fired { Wake::Trigger } else { Wake::Timeout }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The machine's pressure file keeps every byte written but the last,
    /// which it replaces with a NUL; what it parses must be the trigger.
    #[test]
    fn the_written_trigger_survives_the_kernel_dropping_its_last_byte() {
        let (last, kept) = TRIGGER_WRITTEN.split_last().expect("not empty");
        assert_eq!(*last, 0);
        assert_eq!(kept, TRIGGER.as_bytes());
    }

    /// Arming the machine-wide file never fails as malformed. A host may
    /// refuse an unprivileged trigger outright or have no PSI, but a refused
    /// trigger string (`EINVAL`) is this code's fault.
    #[test]
    fn the_machine_wide_trigger_is_never_refused_as_malformed() {
        match arm(Path::new(SYSTEM_PRESSURE)) {
            Ok(_) => {}
            Err(error) => assert_ne!(
                error.raw_os_error(),
                Some(libc::EINVAL),
                "the kernel refused the trigger string: {error}"
            ),
        }
    }
}

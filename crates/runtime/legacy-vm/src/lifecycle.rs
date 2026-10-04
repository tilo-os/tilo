//! When the VM runs, as data.
//!
//! The first launch wakes the VM: it is created from the guest image when it
//! has no disk yet, booted when off, and restored when a snapshot exists.
//! While apps run it stays up. Once nothing has run for the idle limit, the
//! shares are detached and the VM is saved to disk (`virsh managedsave`); the
//! next launch restores it. [`Lifecycle`] only decides; the supervisor carries
//! out the [`Action`]s and reports back.

use libvirt_domain::DomainState;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    /// No disk yet.
    NotCreated,
    /// Defined, powered off, no snapshot.
    Off,
    /// Saved to disk; starting restores it.
    Suspended,
    /// Booting or restoring; the agent has not answered yet.
    Starting,
    Running,
    /// Shares detaching and memory being written out.
    Suspending,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    /// Create the disk as an overlay of the guest image.
    CreateDisk,
    /// Define (or redefine) the domain.
    Define,
    /// `virsh start`, which restores a managed save when there is one.
    Start,
    /// Wait for the guest agent's hello.
    AwaitAgent,
    /// Attach the granted shares and mount them in the guest.
    SyncShares,
    /// Unmount and detach every share: virtio-fs state cannot be saved.
    DetachShares,
    /// `virsh managedsave`.
    Suspend,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum LifecycleError {
    #[error("the VM is being suspended; try again in a moment")]
    Busy,
    #[error("the VM is in a state Tilo does not act on ({0:?}); stop it with `tilo-legacy stop`")]
    Unsafe(DomainState),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Lifecycle {
    phase: Phase,
    /// When the guest was last seen with nothing running.
    idle_since: Option<u64>,
    idle_limit_secs: u64,
}

impl Lifecycle {
    #[must_use]
    pub const fn new(idle_limit_secs: u64) -> Self {
        Self {
            phase: Phase::NotCreated,
            idle_since: None,
            idle_limit_secs,
        }
    }

    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// Take the phase from what libvirt and the disk say.
    pub fn observe(
        &mut self,
        domain: DomainState,
        has_managed_save: bool,
        disk_exists: bool,
    ) -> Result<Phase, LifecycleError> {
        self.phase = match domain {
            _ if !disk_exists => Phase::NotCreated,
            DomainState::ShutOff | DomainState::Crashed if has_managed_save => Phase::Suspended,
            DomainState::Absent | DomainState::ShutOff | DomainState::Crashed => Phase::Off,
            DomainState::Running | DomainState::Starting => match self.phase {
                Phase::Running | Phase::Suspending => self.phase,
                _ => Phase::Starting,
            },
            other @ (DomainState::Paused
            | DomainState::Suspended
            | DomainState::ShuttingDown
            | DomainState::Other) => return Err(LifecycleError::Unsafe(other)),
        };
        Ok(self.phase)
    }

    /// A launch wants the VM. Returns what to do, in order.
    pub fn wake(&mut self) -> Result<Vec<Action>, LifecycleError> {
        self.idle_since = None;
        let actions = match self.phase {
            Phase::NotCreated => vec![
                Action::CreateDisk,
                Action::Define,
                Action::Start,
                Action::AwaitAgent,
                Action::SyncShares,
            ],
            // Redefine on every cold boot so a changed configuration (memory,
            // vCPUs, a new image) takes effect. A snapshot must restore into
            // the definition it was taken with, so a suspended VM is not.
            Phase::Off => vec![
                Action::Define,
                Action::Start,
                Action::AwaitAgent,
                Action::SyncShares,
            ],
            Phase::Suspended => vec![Action::Start, Action::AwaitAgent, Action::SyncShares],
            Phase::Starting => vec![Action::AwaitAgent, Action::SyncShares],
            Phase::Running => vec![Action::SyncShares],
            Phase::Suspending => return Err(LifecycleError::Busy),
        };
        if matches!(
            self.phase,
            Phase::NotCreated | Phase::Off | Phase::Suspended
        ) {
            self.phase = Phase::Starting;
        }
        Ok(actions)
    }

    /// The agent answered.
    pub fn agent_ready(&mut self) {
        if self.phase == Phase::Starting {
            self.phase = Phase::Running;
        }
    }

    /// The guest reported `running` apps at `now` (seconds). Returns the
    /// suspend sequence once it has been idle for the limit.
    pub fn activity(&mut self, running: u32, now: u64) -> Option<Vec<Action>> {
        if self.phase != Phase::Running {
            return None;
        }
        if running > 0 {
            self.idle_since = None;
            return None;
        }
        let since = *self.idle_since.get_or_insert(now);
        if now.saturating_sub(since) < self.idle_limit_secs {
            return None;
        }
        self.phase = Phase::Suspending;
        self.idle_since = None;
        Some(vec![Action::DetachShares, Action::Suspend])
    }

    /// Suspend on request, whatever the idle time.
    pub fn suspend_now(&mut self) -> Option<Vec<Action>> {
        if !matches!(self.phase, Phase::Running | Phase::Starting) {
            return None;
        }
        self.phase = Phase::Suspending;
        self.idle_since = None;
        Some(vec![Action::DetachShares, Action::Suspend])
    }

    /// The managed save finished.
    pub fn suspended(&mut self) {
        self.phase = Phase::Suspended;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vm_wakes_on_launch_and_sleeps_when_idle() {
        // phase: a first launch creates, boots and shares
        {
            let mut life = Lifecycle::new(300);
            assert_eq!(
                life.observe(DomainState::Absent, false, false),
                Ok(Phase::NotCreated)
            );
            assert_eq!(
                life.wake().unwrap(),
                [
                    Action::CreateDisk,
                    Action::Define,
                    Action::Start,
                    Action::AwaitAgent,
                    Action::SyncShares
                ]
            );
            assert_eq!(life.phase(), Phase::Starting);
            assert_eq!(
                life.activity(0, 0),
                None,
                "not idle before the agent answered"
            );
            life.agent_ready();
            assert_eq!(life.phase(), Phase::Running);
            assert_eq!(life.wake().unwrap(), [Action::SyncShares]);
        }
        // phase: idle time accumulates only while nothing runs
        {
            let mut life = Lifecycle::new(300);
            life.observe(DomainState::Running, false, true).unwrap();
            life.agent_ready();
            assert_eq!(life.activity(0, 1_000), None);
            assert_eq!(life.activity(0, 1_200), None);
            assert_eq!(
                life.activity(2, 1_250),
                None,
                "a running app resets the clock"
            );
            assert_eq!(life.activity(0, 1_300), None);
            assert_eq!(life.activity(0, 1_599), None);
            assert_eq!(
                life.activity(0, 1_600),
                Some(vec![Action::DetachShares, Action::Suspend])
            );
            assert_eq!(life.phase(), Phase::Suspending);
            assert_eq!(life.wake(), Err(LifecycleError::Busy));
            life.suspended();
        }
        // phase: a launch restores the snapshot without redefining
        {
            let mut life = Lifecycle::new(300);
            assert_eq!(
                life.observe(DomainState::ShutOff, true, true),
                Ok(Phase::Suspended)
            );
            assert_eq!(
                life.wake().unwrap(),
                [Action::Start, Action::AwaitAgent, Action::SyncShares]
            );
            assert_eq!(
                life.observe(DomainState::ShutOff, false, true),
                Ok(Phase::Off)
            );
            assert_eq!(life.wake().unwrap()[0], Action::Define);
        }
        // phase: states Tilo did not cause are never acted on
        {
            let mut life = Lifecycle::new(300);
            assert_eq!(
                life.observe(DomainState::Paused, false, true),
                Err(LifecycleError::Unsafe(DomainState::Paused))
            );
            assert_eq!(life.suspend_now(), None);
            life.observe(DomainState::Running, false, true).unwrap();
            assert_eq!(
                life.suspend_now(),
                Some(vec![Action::DetachShares, Action::Suspend])
            );
        }
    }
}

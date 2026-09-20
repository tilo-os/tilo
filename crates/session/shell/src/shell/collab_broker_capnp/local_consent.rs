use std::sync::{Arc, Mutex, Weak};

use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LocalSessionLiveness {
    Inactive,
    Active,
    Closed,
}

#[derive(Debug)]
struct LocalSessionState {
    epoch: Uuid,
    generation: u64,
    liveness: LocalSessionLiveness,
    locked: Option<bool>,
    onboarding_active: Option<bool>,
}

pub(crate) struct LocalSessionAuthority {
    state: Arc<Mutex<LocalSessionState>>,
}

#[derive(Clone)]
pub(crate) struct LocalSessionLifecycle {
    state: Weak<Mutex<LocalSessionState>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LiveLocalSession {
    pub(super) epoch: Uuid,
    pub(super) generation: u64,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LocalSessionSnapshot {
    pub(crate) epoch: Uuid,
    pub(crate) generation: u64,
    pub(crate) liveness: LocalSessionLiveness,
    pub(crate) locked: Option<bool>,
    pub(crate) onboarding_active: Option<bool>,
}

impl LocalSessionAuthority {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(LocalSessionState {
                epoch: Uuid::new_v4(),
                generation: 0,
                liveness: LocalSessionLiveness::Inactive,
                locked: None,
                onboarding_active: None,
            })),
        }
    }

    pub(crate) fn lifecycle(&self) -> LocalSessionLifecycle {
        LocalSessionLifecycle {
            state: Arc::downgrade(&self.state),
        }
    }

    pub(super) fn active_session(&self) -> Option<LiveLocalSession> {
        let state = self.state.lock().ok()?;
        (state.liveness == LocalSessionLiveness::Active).then_some(LiveLocalSession {
            epoch: state.epoch,
            generation: state.generation,
        })
    }
}

impl Drop for LocalSessionAuthority {
    fn drop(&mut self) {
        close_state(&self.state);
    }
}

impl LocalSessionLifecycle {
    pub(crate) fn publish_native_state(&self, locked: bool, onboarding_active: bool) {
        self.update(|state| {
            state.locked = Some(locked);
            state.onboarding_active = Some(onboarding_active);
        });
    }

    pub(crate) fn set_locked(&self, locked: bool) {
        self.update(|state| state.locked = Some(locked));
    }

    pub(crate) fn set_onboarding_active(&self, onboarding_active: bool) {
        self.update(|state| state.onboarding_active = Some(onboarding_active));
    }

    pub(crate) fn close(&self) {
        if let Some(state) = self.state.upgrade() {
            close_state(&state);
        }
    }

    fn update(&self, update: impl FnOnce(&mut LocalSessionState)) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.liveness == LocalSessionLiveness::Closed {
            return;
        }
        let before = (state.locked, state.onboarding_active);
        update(&mut state);
        if before == (state.locked, state.onboarding_active) {
            return;
        }
        reconcile(&mut state);
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> Option<LocalSessionSnapshot> {
        let state = self.state.upgrade()?;
        let state = state.lock().ok()?;
        Some(LocalSessionSnapshot {
            epoch: state.epoch,
            generation: state.generation,
            liveness: state.liveness,
            locked: state.locked,
            onboarding_active: state.onboarding_active,
        })
    }
}

fn reconcile(state: &mut LocalSessionState) {
    let eligible = state.locked == Some(false) && state.onboarding_active == Some(false);
    let next = if eligible {
        LocalSessionLiveness::Active
    } else {
        LocalSessionLiveness::Inactive
    };
    if state.liveness != next && !advance_generation(state) {
        state.liveness = LocalSessionLiveness::Closed;
        return;
    }
    state.liveness = next;
}

fn close_state(state: &Mutex<LocalSessionState>) {
    let mut state = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.liveness == LocalSessionLiveness::Closed {
        return;
    }
    let _ = advance_generation(&mut state);
    state.liveness = LocalSessionLiveness::Closed;
}

fn advance_generation(state: &mut LocalSessionState) -> bool {
    let Some(next) = state.generation.checked_add(1) else {
        return false;
    };
    state.generation = next;
    true
}

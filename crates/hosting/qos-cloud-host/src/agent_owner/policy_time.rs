//! Runtime-local clock registration. It owns no durable state or authority.
use qos_action_broker::peer_actions::{OwnerPolicyClock, PolicyOrigin, PolicyTime};
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

pub(crate) struct RuntimePolicyClock {
    clock: OwnerPolicyClock,
    legacy: PolicyOrigin,
    installed: OnceLock<PolicyTime>,
    source: OnceLock<crate::https_owner::CloudStoreSource>,
    closed: AtomicBool,
    started: AtomicBool,
}
impl RuntimePolicyClock {
    pub(crate) fn new(clock: OwnerPolicyClock) -> Self {
        Self {
            legacy: clock.new_origin(),
            clock,
            installed: OnceLock::new(),
            source: OnceLock::new(),
            closed: AtomicBool::new(false),
            started: AtomicBool::new(false),
        }
    }
    pub(crate) fn clock(&self) -> OwnerPolicyClock {
        self.clock.clone()
    }
    pub(crate) fn is_installed(&self) -> bool {
        self.installed.get().is_some()
    }
    pub(crate) fn capture(&self) -> Result<PolicyTime, String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("runtime policy time retired".into());
        }
        let time = match self.installed.get() {
            Some(time) => time.clone(),
            None => self.legacy.capture().map_err(|e| e.to_string())?,
        };
        time.now_ns().map_err(|e| e.to_string())?;
        Ok(time)
    }
    /// Recovery never uses the explicit legacy clock/ambient Store fallback.
    pub(crate) fn capture_installed(&self) -> Result<PolicyTime, String> {
        if self.source.get().is_none() || self.installed.get().is_none() {
            return Err("installed runtime Store Source and policy time required".into());
        }
        self.capture()
    }
    pub(crate) fn validate(&self, time: &PolicyTime) -> Result<(), String> {
        if !time.belongs_to(&self.clock) || !self.capture()?.same_origin(time) {
            return Err("runtime policy clock or source origin differs".into());
        }
        time.now_ns().map_err(|e| e.to_string())?;
        Ok(())
    }
    pub(crate) fn bind_source(&self, time: PolicyTime) -> Result<(), String> {
        if self.closed.load(Ordering::Acquire)
            || self.started.load(Ordering::Acquire)
            || !time.belongs_to(&self.clock)
        {
            return Err("runtime source clock unavailable or different".into());
        }
        time.now_ns().map_err(|e| e.to_string())?;
        self.installed.set(time).map_err(|_| {
            "runtime source already installed; construct a fresh runtime".to_string()
        })?;
        self.legacy.invalidate();
        Ok(())
    }
    pub(crate) fn bind_source_owner(
        &self,
        time: PolicyTime,
        source: crate::https_owner::CloudStoreSource,
    ) -> Result<(), String> {
        self.bind_source(time)?;
        self.source
            .set(source)
            .map_err(|_| "runtime Store source already bound".to_string())
    }
    pub(crate) fn open_store(
        &self,
        root: &std::path::Path,
        machine: &str,
        time: &PolicyTime,
    ) -> Result<crate::Store, String> {
        self.validate(time)?;
        let mut store = if let Some(source) = self.source.get() {
            let store = source
                .open_existing_store(time)
                .map_err(|e| e.to_string())?;
            let scope = store
                .root
                .canonicalize()
                .and_then(|actual| root.canonicalize().map(|expected| actual == expected));
            if !scope.unwrap_or(false) || store.state.machine_id != machine {
                source.retire();
                return Err("runtime Store differs from its bound Source".into());
            }
            store
        } else {
            if !root.join("state.json").is_file() {
                return Err("enrolled Store is absent".into());
            }
            crate::Store::open(root, machine).map_err(|e| e.to_string())?
        };
        store
            .bind_policy_time(time.clone())
            .map_err(|e| e.to_string())?;
        Ok(store)
    }
    pub(crate) fn mark_started(&self) {
        self.started.store(true, Ordering::Release);
    }
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.legacy.invalidate();
        if let Some(source) = self.source.get() {
            source.retire();
        }
    }
}

impl Drop for RuntimePolicyClock {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cloud_policy_clock_runtime_registration_preserves_one_clock_and_is_terminal() {
        let (clock, wall) = OwnerPolicyClock::for_test(200);
        let runtime = RuntimePolicyClock::new(clock.clone());
        let legacy = runtime.capture().unwrap();
        assert!(runtime.validate(&legacy).is_ok());
        let source = clock.new_origin();
        let original = source.capture().unwrap();
        runtime.bind_source(original.clone()).unwrap();
        assert!(runtime.is_installed());
        assert!(runtime.clock().same_clock(&clock));
        assert!(runtime.validate(&legacy).is_err());
        assert!(legacy.now_ns().is_err());
        assert!(runtime.validate(&original).is_ok());
        wall.set_wall_ns(50);
        wall.advance_monotonic(std::time::Duration::from_nanos(1));
        assert_eq!(runtime.capture().unwrap().now_ns().unwrap(), 201);
        source.invalidate();
        assert!(runtime.capture().is_err());
        assert!(runtime.validate(&original).is_err());
        let replacement = clock.new_origin();
        assert!(runtime.bind_source(replacement.capture().unwrap()).is_err());
        assert!(
            runtime.capture().is_err(),
            "retired installed origin fell back to legacy"
        );
    }
    #[test]
    fn cloud_policy_clock_runtime_rejects_other_clock_and_shutdown() {
        let clock = OwnerPolicyClock::new().unwrap();
        let runtime = RuntimePolicyClock::new(clock.clone());
        let alien_clock = OwnerPolicyClock::new().unwrap();
        let alien = alien_clock.new_origin();
        assert!(runtime.bind_source(alien.capture().unwrap()).is_err());
        let source = clock.new_origin();
        runtime.bind_source(source.capture().unwrap()).unwrap();
        let retained = runtime.capture().unwrap();
        runtime.close();
        assert!(runtime.validate(&retained).is_err());
        assert!(runtime.capture().is_err());
    }
}

#[cfg(test)]
#[test]
fn agent_prelaunch_recovery_policy_registration_alone_is_not_a_store_source() {
    let clock = OwnerPolicyClock::new().unwrap();
    let runtime = RuntimePolicyClock::new(clock.clone());
    assert!(runtime.capture_installed().is_err());
    let origin = clock.new_origin();
    runtime.bind_source(origin.capture().unwrap()).unwrap();
    assert!(runtime.capture().is_ok());
    assert!(runtime.capture_installed().is_err());
}
